//! Text layout and glyph rasterization — the only crate in the workspace that knows about fonts.
//!
//! This is the far side of the seam described in [`openpaint_core::text`]. Core owns the
//! *document*: a [`TextBlock`] is a string, a font request and a box. This crate owns the
//! *typography*: matching a family, shaping, breaking lines, and turning glyph outlines into an
//! 8-bit coverage mask. Nothing but [`RenderedText`] crosses back.
//!
//! # Why the seam is a whole crate
//!
//! Layout is a large problem with several credible answers and no permanent one. Putting the font
//! stack behind a module boundary inside `openpaint-core` would have made the seam a convention;
//! putting it in its own crate makes it a fact the compiler enforces — `openpaint-core` cannot
//! reach `parley`, so replacing `parley` cannot reach the document model, the file format, or the
//! renderer.
//!
//! # Why `parley`
//!
//! Over `cosmic-text`, for its span model: per-word styling and Japanese *furigana* are ranges of
//! differently styled text inside one block, which is the shape parley is built around. Both are
//! competent at the horizontal Latin case that lands first, so the tiebreak is what happens after.
//!
//! # Coverage, not colour
//!
//! [`FontStack::render`] returns coverage. The block's colour is applied where the mask becomes
//! tiles, which is the path a selection fill already takes — so text needs no rasterizing pipeline
//! of its own, and inherits correct linear blending and alpha handling rather than reimplementing
//! them.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::path::Path;

use openpaint_core::text::{
    Align, FontResolution, LayoutError, RenderedText, TextBlock, TextRenderer, WritingMode,
};

use parley::{
    Alignment, AlignmentOptions, FontContext, FontFamily, FontFamilyName, FontStyle, FontWeight,
    GenericFamily, Layout, LayoutContext, LineHeight, PositionedLayoutItem, StyleProperty,
};
use swash::scale::{Render, ScaleContext, Source};
use swash::zeno::{Format, Vector};
use swash::FontRef;

/// The most ink a single block may produce, per side, in document pixels.
///
/// A guard rather than a feature. Nothing stops a caller asking for 200pt text in a 40,000 pixel
/// box, and the mask is a flat `Vec<u8>` — one careless number should fail loudly instead of
/// asking the allocator for sixteen gigabytes. Comfortably larger than any real page.
const MAX_MASK_SIDE: u32 = 16_384;

/// Everything needed to turn text into pixels: a font database, layout scratch space, and a
/// glyph scaler.
///
/// Held rather than rebuilt per call because all three are caches. `FontContext` in particular
/// enumerates the system font set on construction, which is slow enough that doing it per keystroke
/// would be felt.
pub struct FontStack {
    fonts: FontContext,
    layout: LayoutContext<()>,
    scale: ScaleContext,
}

impl Default for FontStack {
    fn default() -> Self {
        Self::new()
    }
}

impl FontStack {
    /// Build the stack, discovering the fonts installed on this machine.
    #[must_use]
    pub fn new() -> Self {
        Self {
            fonts: FontContext::new(),
            layout: LayoutContext::new(),
            scale: ScaleContext::new(),
        }
    }

    /// Every font family available, sorted and deduplicated, for a font picker.
    ///
    /// Sorted here rather than at the call site because there is exactly one sensible order and no
    /// reason for each caller to rediscover it. Deduplicated because a family reachable from more
    /// than one source should appear once.
    pub fn families(&mut self) -> Vec<String> {
        let unique: BTreeSet<String> = self
            .fonts
            .collection
            .family_names()
            .map(str::to_owned)
            .collect();
        unique.into_iter().collect()
    }

    /// Register font files that are not installed on the system.
    ///
    /// Comic letterers keep collections of fonts they have no wish to install, and asking someone
    /// to pollute their OS font list to letter one page is the wrong trade. Returns the family
    /// names that became available.
    pub fn load_font_files<P: AsRef<Path>>(
        &mut self,
        paths: impl IntoIterator<Item = P>,
    ) -> Vec<String> {
        let before: BTreeSet<String> = self
            .fonts
            .collection
            .family_names()
            .map(str::to_owned)
            .collect();
        self.fonts.collection.load_fonts_from_paths(paths);
        self.fonts
            .collection
            .family_names()
            .filter(|name| !before.contains(*name))
            .map(str::to_owned)
            .collect()
    }

    /// Lay out a block, without rasterizing it.
    ///
    /// Split out because the text tool needs the geometry — line boxes, caret positions, the
    /// block's measured size — at times when it does not need pixels, and laying out is much the
    /// cheaper half.
    fn lay_out<'a>(&'a mut self, block: &'a TextBlock) -> Layout<()> {
        let size = block.size_px();
        let mut builder = self
            .layout
            .ranged_builder(&mut self.fonts, &block.text, 1.0, true);

        // A named family with a generic *behind* it, so a document that asks for a font this
        // machine does not have still gets something rather than nothing. The fallback is the
        // reason a travelling document still renders; `FontResolution` is the reason the reader
        // finds out it did.
        let family = if block.font.family.is_empty() {
            FontFamily::Single(FontFamilyName::Generic(GenericFamily::SansSerif))
        } else {
            FontFamily::List(Cow::Owned(vec![
                FontFamilyName::Named(Cow::Borrowed(block.font.family.as_str())),
                FontFamilyName::Generic(GenericFamily::SansSerif),
            ]))
        };
        builder.push_default(StyleProperty::FontFamily(family));
        builder.push_default(StyleProperty::FontSize(size));
        builder.push_default(StyleProperty::FontWeight(FontWeight::new(f32::from(
            block.font.weight.clamp(1, 1000),
        ))));
        builder.push_default(StyleProperty::FontStyle(if block.font.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        }));
        // Relative to the font size, matching how `TextBlock::line_height` is defined, so the two
        // cannot drift apart.
        builder.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(
            block.line_height.max(0.0),
        )));
        builder.push_default(StyleProperty::LetterSpacing(block.letter_spacing));

        let mut layout: Layout<()> = builder.build(&block.text);
        layout.break_all_lines(block.wrap_width);
        layout.align(
            match block.align {
                Align::Start => Alignment::Start,
                Align::Center => Alignment::Center,
                Align::End => Alignment::End,
            },
            AlignmentOptions::default(),
        );
        layout
    }
}

/// One rasterized glyph, and where its top-left corner sits in layout space.
struct Stamp {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    coverage: Vec<u8>,
}

impl TextRenderer for FontStack {
    fn render(&mut self, block: &TextBlock) -> Result<RenderedText, LayoutError> {
        if block.writing_mode != WritingMode::Horizontal {
            return Err(LayoutError::UnsupportedWritingMode(block.writing_mode));
        }

        let requested = block.font.family.clone();
        if block.is_blank() {
            // Still resolve the font, so an empty layer knows whether its font is missing — that
            // is exactly the layer someone is about to type into.
            let resolved = self.resolve_family(&requested);
            return Ok(RenderedText::empty(FontResolution {
                requested,
                resolved,
            }));
        }

        let layout = self.lay_out(block);

        // Collect every glyph's mask first, because the extent of the ink is not known until the
        // last one is rasterized: glyphs overhang their advances, and a descender or an italic tail
        // can reach outside the layout box in either direction.
        let mut stamps: Vec<Stamp> = Vec::new();
        let mut resolved = String::new();
        for line in layout.lines() {
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(run) = item else {
                    continue;
                };
                let font = run.run().font();
                let Some(font_ref) = FontRef::from_index(font.data.data(), font.index as usize)
                else {
                    continue;
                };
                if resolved.is_empty() {
                    resolved = family_name(&font_ref);
                }

                let synthesis = run.run().synthesis();
                let mut scaler = self
                    .scale
                    .builder(font_ref)
                    .size(run.run().font_size())
                    .hint(false)
                    .normalized_coords(run.run().normalized_coords())
                    .build();

                let mut pen_x = run.offset();
                let pen_y = run.baseline();
                for glyph in run.glyphs() {
                    let x = pen_x + glyph.x;
                    let y = pen_y - glyph.y;
                    pen_x += glyph.advance;

                    // Rasterize at the glyph's subpixel phase rather than snapping to the pixel
                    // grid. Snapping would make letter spacing visibly uneven at the sizes lettering
                    // is actually set at.
                    let (ix, fx) = split(x);
                    let (iy, fy) = split(y);

                    let mut render = Render::new(&[Source::Outline]);
                    render.format(Format::Alpha).offset(Vector::new(fx, fy));
                    if synthesis.embolden() {
                        // Faux bold, for a family with no real bold cut. Proportional to the size,
                        // or it would be invisible on a caption and a blob on a title.
                        render.embolden(run.run().font_size() * 0.02);
                    }
                    if let Some(skew) = synthesis.skew() {
                        render.transform(Some(swash::zeno::Transform::skew(
                            swash::zeno::Angle::from_degrees(skew),
                            swash::zeno::Angle::ZERO,
                        )));
                    }

                    let Some(image) = render.render(&mut scaler, glyph.id as u16) else {
                        continue;
                    };
                    if image.placement.width == 0 || image.placement.height == 0 {
                        continue;
                    }
                    stamps.push(Stamp {
                        left: ix + image.placement.left,
                        top: iy - image.placement.top,
                        width: image.placement.width,
                        height: image.placement.height,
                        coverage: image.data,
                    });
                }
            }
        }

        let font = FontResolution {
            requested,
            resolved,
        };
        Ok(compose(&stamps, block.x, block.y, font))
    }
}

impl FontStack {
    /// What family a request actually resolves to on this machine.
    ///
    /// Asked of the collection rather than of a laid-out run, because a blank block has no runs and
    /// still needs to know whether its font is present.
    fn resolve_family(&mut self, requested: &str) -> String {
        if requested.is_empty() {
            return String::new();
        }
        self.fonts
            .collection
            .family_by_name(requested)
            .map(|family| family.name().to_owned())
            .unwrap_or_default()
    }
}

/// Split a coordinate into a whole pixel and the fraction within it.
///
/// `floor`, not `trunc`: at a negative coordinate `trunc` rounds toward zero, which would hand the
/// rasterizer a negative phase and shift that glyph a pixel the wrong way. Text starting left of
/// the origin is ordinary — a box dragged leftwards, or a glyph overhanging its own start.
fn split(v: f32) -> (i32, f32) {
    let whole = v.floor();
    #[expect(
        clippy::cast_possible_truncation,
        reason = "coordinates are page pixels, far inside i32"
    )]
    (whole as i32, v - whole)
}

/// Union a set of glyph stamps into one mask, offset to its place on the page.
fn compose(stamps: &[Stamp], x: f32, y: f32, font: FontResolution) -> RenderedText {
    if stamps.is_empty() {
        return RenderedText::empty(font);
    }

    let min_x = stamps.iter().map(|s| s.left).min().unwrap_or(0);
    let min_y = stamps.iter().map(|s| s.top).min().unwrap_or(0);
    #[expect(
        clippy::cast_possible_wrap,
        reason = "glyph masks are bounded by MAX_MASK_SIDE"
    )]
    let max_x = stamps
        .iter()
        .map(|s| s.left + s.width as i32)
        .max()
        .unwrap_or(0);
    #[expect(
        clippy::cast_possible_wrap,
        reason = "glyph masks are bounded by MAX_MASK_SIDE"
    )]
    let max_y = stamps
        .iter()
        .map(|s| s.top + s.height as i32)
        .max()
        .unwrap_or(0);

    #[expect(
        clippy::cast_sign_loss,
        reason = "max is derived from min plus a width, so the difference is non-negative"
    )]
    let width = ((max_x - min_x) as u32).min(MAX_MASK_SIDE);
    #[expect(
        clippy::cast_sign_loss,
        reason = "max is derived from min plus a height, so the difference is non-negative"
    )]
    let height = ((max_y - min_y) as u32).min(MAX_MASK_SIDE);
    if width == 0 || height == 0 {
        return RenderedText::empty(font);
    }

    let mut coverage = vec![0_u8; width as usize * height as usize];
    for stamp in stamps {
        for row in 0..stamp.height {
            let dy = stamp.top - min_y + row as i32;
            if dy < 0 || dy >= height as i32 {
                continue;
            }
            for col in 0..stamp.width {
                let dx = stamp.left - min_x + col as i32;
                if dx < 0 || dx >= width as i32 {
                    continue;
                }
                let src = stamp.coverage[(row * stamp.width + col) as usize];
                if src == 0 {
                    continue;
                }
                let dst = &mut coverage[dy as usize * width as usize + dx as usize];
                // Source-over on coverage, not `max`. Glyphs overlap — kerned pairs, accents, a
                // script face's connecting strokes — and `max` would leave a visible seam where two
                // antialiased edges meet instead of filling the join.
                *dst = over(src, *dst);
            }
        }
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "page coordinates, far inside i32"
    )]
    let (page_x, page_y) = (x.floor() as i32, y.floor() as i32);
    RenderedText::new(
        page_x + min_x,
        page_y + min_y,
        width,
        height,
        coverage,
        font,
    )
}

/// `src` over `dst`, on 8-bit coverage.
///
/// Rounded rather than truncated, so a long run of small contributions accumulates to full coverage
/// instead of stalling one short of it.
fn over(src: u8, dst: u8) -> u8 {
    let (s, d) = (u32::from(src), u32::from(dst));
    #[expect(clippy::cast_possible_truncation, reason = "the result is <= 255")]
    {
        (s + (d * (255 - s) + 127) / 255) as u8
    }
}

/// The family name recorded inside a font file.
///
/// Read from the font rather than from whatever was asked for, because the whole point is to report
/// what was *actually* used when the request could not be honoured.
fn family_name(font: &FontRef<'_>) -> String {
    font.localized_strings()
        .find_by_id(swash::StringId::Family, None)
        .map(|name| name.to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Coverage compositing has to reach the ends exactly, or text made of many overlapping strokes
    /// never quite becomes solid.
    #[test]
    fn coverage_over_reaches_both_ends() {
        assert_eq!(over(0, 0), 0);
        assert_eq!(over(255, 0), 255);
        assert_eq!(over(0, 255), 255);
        assert_eq!(over(255, 255), 255);
    }

    /// Two half-covered edges meeting must darken, which is the behaviour `max` would not give.
    #[test]
    fn coverage_over_accumulates() {
        let once = over(128, 0);
        let twice = over(128, once);
        assert!(twice > once, "a second stroke has to add to the first");
        assert!(twice < 255);
        assert_eq!(twice, over(128, 128));
    }

    /// Repeated small contributions climb, and settle just short of solid.
    ///
    /// Just short, not exactly solid: in eight bits the last step is smaller than one level, so
    /// `over` has a fixed point a couple of levels below 255 however it rounds. That is harmless
    /// for text — a glyph's interior arrives at full coverage in one stamp, and the only place this
    /// accumulates is an antialiased edge, which is meant to be partial. The test pins the shape of
    /// the behaviour rather than a number it cannot reach.
    #[test]
    fn coverage_over_converges_on_solid() {
        let mut c = 0_u8;
        let mut previous = 0_u8;
        for step in 0..64 {
            c = over(40, c);
            assert!(
                c >= previous,
                "coverage went backwards at step {step}: {previous} -> {c}"
            );
            previous = c;
        }
        assert!(c > 250, "sixty-four stamps only reached {c}");

        // A solid stamp does arrive at solid, which is the case that actually renders text.
        assert_eq!(over(255, c), 255);
    }

    /// Negative coordinates are ordinary — a box dragged leftwards, a glyph overhanging its start —
    /// and `trunc` would shift those glyphs a pixel the wrong way.
    #[test]
    fn splitting_a_coordinate_floors_rather_than_truncating() {
        let (i, f) = split(3.25);
        assert_eq!(i, 3);
        assert!((f - 0.25).abs() < 1e-6);

        let (i, f) = split(-3.25);
        assert_eq!(i, -4, "floor, not trunc");
        assert!(
            (0.0..1.0).contains(&f),
            "the phase must stay in [0,1), got {f}"
        );
        assert!((f - 0.75).abs() < 1e-6);
    }

    fn stamp(left: i32, top: i32, width: u32, height: u32, value: u8) -> Stamp {
        Stamp {
            left,
            top,
            width,
            height,
            coverage: vec![value; (width * height) as usize],
        }
    }

    /// The mask is sized to the ink and placed on the page, so nothing downstream has to know how
    /// far a glyph overhung its own box.
    #[test]
    fn composing_bounds_the_ink_and_offsets_to_the_page() {
        let stamps = [stamp(2, 3, 4, 5, 200)];
        let out = compose(&stamps, 100.0, 200.0, FontResolution::default());
        assert_eq!(out.bounds(), (102, 203, 4, 5));
        assert_eq!(out.coverage_at(102, 203), 200);
        assert_eq!(out.coverage_at(105, 207), 200);
        assert_eq!(out.coverage_at(106, 203), 0, "one past the right edge");
    }

    /// Ink to the left of and above the block's own origin has to survive, because a glyph's
    /// bearings routinely put it there.
    #[test]
    fn composing_keeps_ink_outside_the_origin() {
        let stamps = [stamp(-3, -2, 2, 2, 128)];
        let out = compose(&stamps, 10.0, 10.0, FontResolution::default());
        assert_eq!(out.bounds(), (7, 8, 2, 2));
        assert_eq!(out.coverage_at(7, 8), 128);
    }

    /// Two glyphs make one mask spanning both, with the gap between them empty.
    #[test]
    fn composing_unions_separated_glyphs() {
        let stamps = [stamp(0, 0, 2, 2, 255), stamp(10, 0, 2, 2, 255)];
        let out = compose(&stamps, 0.0, 0.0, FontResolution::default());
        assert_eq!(out.bounds(), (0, 0, 12, 2));
        assert_eq!(out.coverage_at(0, 0), 255);
        assert_eq!(out.coverage_at(11, 1), 255);
        assert_eq!(out.coverage_at(5, 0), 0, "the gap must stay empty");
    }

    /// Where glyphs overlap, coverage combines rather than one replacing the other.
    #[test]
    fn composing_overlapping_glyphs_darkens_the_join() {
        let stamps = [stamp(0, 0, 2, 2, 128), stamp(1, 0, 2, 2, 128)];
        let out = compose(&stamps, 0.0, 0.0, FontResolution::default());
        assert_eq!(out.bounds(), (0, 0, 3, 2));
        assert_eq!(out.coverage_at(0, 0), 128, "left glyph alone");
        assert_eq!(out.coverage_at(2, 0), 128, "right glyph alone");
        assert!(
            out.coverage_at(1, 0) > 128,
            "the overlap should be denser than either, got {}",
            out.coverage_at(1, 0)
        );
    }

    #[test]
    fn composing_nothing_is_an_empty_mask() {
        let out = compose(&[], 5.0, 5.0, FontResolution::default());
        assert!(out.is_empty());
        assert_eq!(out.coverage_at(5, 5), 0);
    }

    /// Vertical writing is reserved, not implemented, and must say so rather than silently
    /// laying a manga page out sideways.
    #[test]
    fn vertical_writing_is_refused_rather_than_faked() {
        let mut stack = FontStack::new();
        let block = TextBlock {
            text: "縦書き".into(),
            writing_mode: WritingMode::VerticalRightToLeft,
            ..TextBlock::default()
        };
        assert_eq!(
            stack.render(&block),
            Err(LayoutError::UnsupportedWritingMode(
                WritingMode::VerticalRightToLeft
            ))
        );
    }

    /// The end-to-end path, against whatever fonts this machine has.
    #[test]
    fn text_rasterizes_to_ink() {
        let mut stack = FontStack::new();
        if stack.families().is_empty() {
            eprintln!("skipping: no fonts installed");
            return;
        }

        let block = TextBlock {
            text: "Hamburgefonstiv".into(),
            size: 48.0,
            x: 100.0,
            y: 200.0,
            ..TextBlock::default()
        };
        let out = stack
            .render(&block)
            .expect("horizontal text should lay out");

        assert!(!out.is_empty(), "no ink was produced");
        let (_, _, w, h) = out.bounds();
        assert!(w > 100, "fifteen glyphs at 48px should be wider than {w}");
        assert!(
            (20..200).contains(&h),
            "48px text should be tens of pixels tall, not {h}"
        );
        assert!(
            out.coverage.iter().any(|&c| c > 200),
            "an antialiased mask with no solid pixel anywhere is a rasterizer that did not run"
        );

        // The mask has to land near where the block was placed, or every caller's offset
        // arithmetic is wrong in a way no smaller test would show.
        let (mx, my, _, _) = out.bounds();
        assert!(
            (90..140).contains(&mx),
            "mask x {mx} is nowhere near the block's 100"
        );
        assert!(
            (190..260).contains(&my),
            "mask y {my} is nowhere near the block's 200"
        );
    }

    /// Bigger text makes bigger ink. Cheap, and it catches a size that never reached the scaler —
    /// which is otherwise invisible, because the output still looks like text.
    #[test]
    fn size_reaches_the_rasterizer() {
        let mut stack = FontStack::new();
        if stack.families().is_empty() {
            eprintln!("skipping: no fonts installed");
            return;
        }
        let at = |stack: &mut FontStack, size| {
            let block = TextBlock {
                text: "Wg".into(),
                size,
                ..TextBlock::default()
            };
            stack.render(&block).expect("lays out").bounds()
        };
        let (_, _, small_w, small_h) = at(&mut stack, 12.0);
        let (_, _, big_w, big_h) = at(&mut stack, 96.0);
        assert!(
            big_w > small_w * 4 && big_h > small_h * 4,
            "8x the size gave {small_w}x{small_h} -> {big_w}x{big_h}"
        );
    }

    /// Wrapping has to actually wrap, or a caption box is decoration.
    #[test]
    fn a_wrap_width_makes_more_lines() {
        let mut stack = FontStack::new();
        if stack.families().is_empty() {
            eprintln!("skipping: no fonts installed");
            return;
        }
        let text = "the quick brown fox jumps over the lazy dog";
        let unwrapped = stack
            .render(&TextBlock {
                text: text.into(),
                size: 20.0,
                ..TextBlock::default()
            })
            .expect("lays out");
        let wrapped = stack
            .render(&TextBlock {
                text: text.into(),
                size: 20.0,
                wrap_width: Some(120.0),
                ..TextBlock::default()
            })
            .expect("lays out");

        let (_, _, uw, uh) = unwrapped.bounds();
        let (_, _, ww, wh) = wrapped.bounds();
        assert!(ww < uw, "wrapped text should be narrower: {ww} vs {uw}");
        assert!(wh > uh * 2, "wrapped text should be taller: {wh} vs {uh}");
        assert!(
            ww <= 130,
            "a 120px wrap width produced a {ww}px line, so it was ignored"
        );
    }

    /// A blank block still has to report its font, because that is the layer about to be typed in.
    #[test]
    fn a_blank_block_renders_nothing_but_resolves_its_font() {
        let mut stack = FontStack::new();
        let out = stack
            .render(&TextBlock {
                text: "   ".into(),
                font: openpaint_core::text::FontSpec::family("No Such Font Exists 12345"),
                ..TextBlock::default()
            })
            .expect("blank text is not an error");
        assert!(out.is_empty());
        assert!(
            out.font.is_substituted(),
            "a font that does not exist should be reported as substituted, got {:?}",
            out.font
        );
    }

    /// The reason `FontSpec` is a request rather than a resolution: documents travel to machines
    /// that do not have the font, and that must be visible rather than silent.
    #[test]
    fn a_missing_font_is_reported_not_hidden() {
        let mut stack = FontStack::new();
        if stack.families().is_empty() {
            eprintln!("skipping: no fonts installed");
            return;
        }
        let out = stack
            .render(&TextBlock {
                text: "Hello".into(),
                font: openpaint_core::text::FontSpec::family("Definitely Not Installed 98765"),
                ..TextBlock::default()
            })
            .expect("lays out with a fallback");
        assert!(!out.is_empty(), "it should still draw something");
        assert!(
            out.font.is_substituted(),
            "requested {:?}, got {:?}, and that was not reported",
            out.font.requested,
            out.font.resolved
        );
    }

    #[test]
    fn the_installed_families_are_sorted_and_unique() {
        let mut stack = FontStack::new();
        let families = stack.families();
        if families.is_empty() {
            eprintln!("skipping: no fonts installed");
            return;
        }
        let mut sorted = families.clone();
        sorted.sort();
        assert_eq!(families, sorted, "the picker list should arrive in order");
        sorted.dedup();
        assert_eq!(families.len(), sorted.len(), "duplicate families listed");
    }
}
