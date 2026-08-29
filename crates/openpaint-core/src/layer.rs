//! Layers: the stack of surfaces a page is drawn on.
//!
//! A layer here is **properties only** — name, opacity, blend mode, visibility. Its pixels
//! live in the renderer's tile store, keyed by the layer's id, because the GPU is
//! authoritative for pixels (`docs/DECISIONS.md` §4a) and residency is a renderer concern.
//! Keeping the two apart is what lets a layer be reordered, hidden or renamed without any
//! pixel moving.
//!
//! # Ids are stable; positions are not
//!
//! A layer's [`Layer::id`] is assigned once and never reused. Its *position* in the stack is
//! just its index, which changes whenever the stack is reordered. Tiles are keyed by id
//! precisely so that reordering a stack is a `Vec` operation and not a rekeying of every
//! tile the document owns.
//!
//! # Blend modes
//!
//! [`Blend::apply`] is the separable colour function from the PDF/CSS compositing model,
//! which is what Photoshop and CSP implement — so "Multiply" here means the same thing an
//! artist already expects it to mean. It takes and returns **straight** (un-premultiplied)
//! colour, because that is the space the blend functions are defined in; the compositor
//! un-premultiplies, blends, and re-premultiplies around it. The GPU has a second copy of
//! this in `composite.wgsl`, and [`Blend::apply`] is what the tests compare it against.

use crate::text::TextBlock;

/// How a layer combines with what is beneath it.
///
/// Deliberately a small set. These three cover the overwhelming majority of comic and
/// illustration work — ink on Normal, shadows on Multiply, highlights on Screen — and each
/// additional mode is a line in two places that has to stay in step. More can be added when
/// a real workflow wants them, rather than to fill out a menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Blend {
    #[default]
    Normal,
    Multiply,
    Screen,
}

impl Blend {
    /// Every mode, in the order the UI lists them.
    pub const ALL: [Self; 3] = [Self::Normal, Self::Multiply, Self::Screen];

    /// The name shown in the UI.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Multiply => "Multiply",
            Self::Screen => "Screen",
        }
    }

    /// The shader's encoding of this mode, matching `composite.wgsl`.
    #[must_use]
    pub fn code(self) -> u32 {
        match self {
            Self::Normal => 0,
            Self::Multiply => 1,
            Self::Screen => 2,
        }
    }

    /// The separable blend function, on **straight** (un-premultiplied) channels.
    ///
    /// `src` is the layer, `dst` is what is already underneath.
    #[must_use]
    pub fn apply(self, src: f32, dst: f32) -> f32 {
        match self {
            Self::Normal => src,
            Self::Multiply => src * dst,
            // 1 - (1-a)(1-b): the complement of multiplying the inverses.
            Self::Screen => src + dst - src * dst,
        }
    }
}

/// What a layer is *made of* — the thing its pixels are derived from.
///
/// The distinction that matters is **who owns the truth**:
///
/// - [`Content::Raster`]: the tiles are the truth. A brush writes them and nothing regenerates
///   them, which is why a painted stroke survives forever.
/// - [`Content::Text`]: the [`TextBlock`] is the truth and the tiles are a *cache* of it, thrown
///   away and rebuilt whenever the text, font or box changes. That is what lets a caption typed on
///   Monday be retyped on Thursday.
///
/// Everything downstream is deliberately untouched by this. The compositor reads tiles keyed by
/// layer id and neither knows nor cares which of these filled them, so blend modes, opacity,
/// clipping, alpha lock, selection and export need no text-specific code at all. The one thing that
/// does change is who may write those tiles: see [`Layer::accepts_paint`].
///
/// This enum is also where vector content lands later. It is the reason the text work does not have
/// to be redone to get there — the shape of the answer is already "a layer has a source of truth",
/// and a vector layer is another arm of it.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum Content {
    #[default]
    Raster,
    /// Boxed because a [`TextBlock`] is far larger than the unit variant, and a `Layer` is cloned
    /// for every history snapshot; without the box every raster layer would pay text's size.
    Text(Box<TextBlock>),
}

impl Content {
    /// The name shown where a layer's kind is displayed.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Raster => "Raster",
            Self::Text(_) => "Text",
        }
    }
}

/// One layer of a page: how it looks, not what is on it.
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    id: u32,
    pub name: String,
    /// 0..=1, applied to the whole layer at composite time.
    pub opacity: f32,
    pub blend: Blend,
    pub visible: bool,
    /// Freeze this layer's transparency. See [`Layer::locks_alpha`].
    pub lock_alpha: bool,
    /// Mask this layer by the layer below it. See [`Layer::clips_below`].
    pub clip_below: bool,
    /// What this layer is made of. See [`Content`].
    ///
    /// Private, and reached through [`Layer::content`] / [`Layer::set_content`], because changing it
    /// is not an ordinary field write: a layer whose content becomes [`Content::Text`] stops
    /// accepting paint, and one that becomes [`Content::Raster`] has just had its text discarded.
    /// Both are decisions, and the type should make them look like decisions.
    content: Content,
}

impl Layer {
    /// Rebuild a layer exactly as it was, for loading a document.
    ///
    /// Takes the id rather than assigning one, because tiles are keyed by it: a load that
    /// renumbered layers would separate every layer from its pixels. Deliberately the only way
    /// to choose an id from outside this crate, so ordinary code cannot invent one.
    #[must_use]
    pub fn restored(
        id: u32,
        name: impl Into<String>,
        opacity: f32,
        blend: Blend,
        visible: bool,
        lock_alpha: bool,
        clip_below: bool,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            opacity,
            blend,
            visible,
            lock_alpha,
            clip_below,
            content: Content::Raster,
        }
    }

    /// The same layer, carrying text instead of pixels.
    ///
    /// Separate from [`Layer::restored`] rather than an eighth positional argument to it: that call
    /// already takes three bools in a row, and a loader that has no text to restore should not have
    /// to say so.
    #[must_use]
    pub fn with_text(mut self, block: TextBlock) -> Self {
        self.content = Content::Text(Box::new(block));
        self
    }

    /// Create a layer with a given id. Only [`crate::page::Page`] hands out ids, so that
    /// they stay unique within a page.
    #[must_use]
    pub(crate) fn new(id: u32, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            opacity: 1.0,
            blend: Blend::Normal,
            visible: true,
            lock_alpha: false,
            clip_below: false,
            content: Content::Raster,
        }
    }

    /// Stable for the layer's whole life, and never reused. Tiles are keyed by it.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Whether this layer's transparency is frozen.
    ///
    /// **The definition is exactly that: alpha cannot change.** Painting is confined to pixels that
    /// already have coverage, which is how colour goes inside line art without a selection — and it
    /// also means an eraser cannot remove anything here, because removing is a change in alpha. A
    /// looser definition ("painting is masked, erasing still works") would make the guarantee
    /// conditional on which tool you happened to be holding, which is not a guarantee.
    #[must_use]
    pub fn locks_alpha(&self) -> bool {
        self.lock_alpha
    }

    /// Whether this layer is masked by the one below it.
    ///
    /// The *non-destructive* counterpart of [`Layer::locks_alpha`], and the one the colouring
    /// workflow actually rests on: shading and highlight layers clipped to a layer of flats stay
    /// separately editable — their own opacity, their own blend mode, erasable without touching the
    /// flats. Alpha lock bakes the same paint into the layer it masks, which is destructive and
    /// therefore useful for fewer things.
    ///
    /// A run of consecutive clipped layers all clip to the nearest unclipped layer beneath them —
    /// the *base* of the group. A clipped layer with no unclipped layer below it has nothing to clip
    /// to and shows nothing, which is the only consistent reading.
    #[must_use]
    pub fn clips_below(&self) -> bool {
        self.clip_below
    }

    /// What this layer is made of.
    #[must_use]
    pub fn content(&self) -> &Content {
        &self.content
    }

    /// Replace what this layer is made of.
    ///
    /// Note what this does *not* do: it does not touch tiles. Those live in the renderer, and a
    /// content change invalidates them rather than rewriting them — the caller re-derives. Doing it
    /// here would put the renderer inside the document model, which is the one thing this crate is
    /// meant not to know about.
    pub fn set_content(&mut self, content: Content) {
        self.content = content;
    }

    /// The text this layer holds, if it holds text.
    #[must_use]
    pub fn text(&self) -> Option<&TextBlock> {
        match &self.content {
            Content::Raster => None,
            Content::Text(block) => Some(block),
        }
    }

    /// The text this layer holds, to edit in place.
    ///
    /// The tiles are stale the moment this is written through, so whatever takes it is responsible
    /// for re-deriving them.
    pub fn text_mut(&mut self) -> Option<&mut TextBlock> {
        match &mut self.content {
            Content::Raster => None,
            Content::Text(block) => Some(block),
        }
    }

    /// Whether a brush may write to this layer's pixels.
    ///
    /// **The rule that makes derived content safe.** A stroke painted on a text layer would vanish
    /// the next time the text was re-rendered — not immediately, but the next time someone fixed a
    /// typo, which is far worse than refusing outright. So the answer is no, and the UI offers to
    /// convert the layer to raster instead, which is the same bargain CSP strikes.
    ///
    /// Asked as a question about the layer rather than checked at each tool, so a tool added later
    /// cannot forget: painting, filling, clearing, erasing and moving all go through this.
    #[must_use]
    pub fn accepts_paint(&self) -> bool {
        matches!(self.content, Content::Raster)
    }

    /// Opacity clamped to the range the compositor accepts.
    #[must_use]
    pub fn effective_opacity(&self) -> f32 {
        if self.visible {
            self.opacity.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default is what every layer has always been, so adding a content kind changed no
    /// existing layer.
    #[test]
    fn a_layer_is_raster_unless_told_otherwise() {
        let l = Layer::new(1, "Layer 1");
        assert_eq!(*l.content(), Content::Raster);
        assert!(l.text().is_none());
        assert!(l.accepts_paint());
    }

    /// The rule that keeps derived content safe: a brush must never write pixels that a later
    /// re-render would discard.
    #[test]
    fn a_text_layer_refuses_paint() {
        let l = Layer::new(1, "Caption").with_text(TextBlock::at(10.0, 20.0));
        assert!(
            !l.accepts_paint(),
            "painting here would vanish the next time the text was edited"
        );
        assert!(l.text().is_some());
        assert_eq!(l.content().label(), "Text");
    }

    /// Converting to raster is what the UI offers instead of refusing forever, and it has to
    /// actually restore painting.
    #[test]
    fn converting_to_raster_gives_paint_back_and_drops_the_text() {
        let mut l = Layer::new(1, "Caption").with_text(TextBlock::at(0.0, 0.0));
        l.set_content(Content::Raster);
        assert!(l.accepts_paint());
        assert!(
            l.text().is_none(),
            "the text is gone, which is what makes this one-way"
        );
    }

    /// Content travels with the layer, so a history snapshot restores the text as well as the
    /// name and blend mode.
    #[test]
    fn content_is_part_of_a_layers_identity() {
        let mut a = Layer::new(1, "Caption").with_text(TextBlock::at(0.0, 0.0));
        let b = a.clone();
        assert_eq!(a, b);

        a.text_mut().expect("text layer").text = "Hello".into();
        assert_ne!(a, b, "an edit to the text has to be a change to the layer");
        assert_eq!(b.text().expect("still text").text, "");
    }

    /// Everything that is not pixels keeps working on a text layer, because nothing downstream
    /// looks at the content kind.
    #[test]
    fn a_text_layer_is_still_an_ordinary_layer() {
        let mut l = Layer::new(1, "Caption").with_text(TextBlock::at(0.0, 0.0));
        l.blend = Blend::Multiply;
        l.opacity = 0.5;
        assert!((l.effective_opacity() - 0.5).abs() < 1e-6);
        l.visible = false;
        assert_eq!(l.effective_opacity(), 0.0);
    }

    #[test]
    fn normal_ignores_what_is_underneath() {
        assert_eq!(Blend::Normal.apply(0.25, 0.75), 0.25);
    }

    /// Multiply darkens and can never lighten, which is the property that makes it the right
    /// mode for shadow layers.
    #[test]
    fn multiply_only_darkens() {
        assert_eq!(
            Blend::Multiply.apply(1.0, 0.5),
            0.5,
            "white leaves it alone"
        );
        assert_eq!(
            Blend::Multiply.apply(0.0, 0.5),
            0.0,
            "black takes it to black"
        );
        for (s, d) in [(0.3, 0.8), (0.6, 0.6), (0.9, 0.1)] {
            assert!(
                Blend::Multiply.apply(s, d) <= d + 1e-6,
                "{s} over {d} lightened"
            );
        }
    }

    /// Screen lightens and can never darken -- the mirror of Multiply, for highlights.
    #[test]
    fn screen_only_lightens() {
        assert_eq!(Blend::Screen.apply(0.0, 0.5), 0.5, "black leaves it alone");
        assert_eq!(
            Blend::Screen.apply(1.0, 0.5),
            1.0,
            "white takes it to white"
        );
        for (s, d) in [(0.3, 0.8), (0.6, 0.6), (0.9, 0.1)] {
            assert!(
                Blend::Screen.apply(s, d) >= d - 1e-6,
                "{s} over {d} darkened"
            );
        }
    }

    /// Screen is Multiply on the inverted channels. If one of them is ever changed without
    /// the other, this is what notices.
    #[test]
    fn screen_is_multiply_inverted() {
        for (s, d) in [(0.0, 0.0), (0.25, 0.5), (0.9, 0.3), (1.0, 1.0)] {
            let via_multiply = 1.0 - Blend::Multiply.apply(1.0 - s, 1.0 - d);
            assert!((Blend::Screen.apply(s, d) - via_multiply).abs() < 1e-6);
        }
    }

    /// The shader encoding has to be dense and unique, since `composite.wgsl` switches on it.
    #[test]
    fn blend_codes_are_unique_and_dense() {
        let codes: Vec<u32> = Blend::ALL.iter().map(|b| b.code()).collect();
        let mut sorted = codes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len(), "duplicate blend code");
        assert_eq!(sorted, (0..Blend::ALL.len() as u32).collect::<Vec<_>>());
    }

    /// A hidden layer must contribute nothing, and that has to be true through one accessor
    /// rather than every call site remembering to check `visible`.
    #[test]
    fn a_hidden_layer_contributes_nothing() {
        let mut l = Layer::new(0, "test");
        assert_eq!(l.effective_opacity(), 1.0);
        l.visible = false;
        assert_eq!(l.effective_opacity(), 0.0);
    }

    #[test]
    fn opacity_is_clamped() {
        let mut l = Layer::new(0, "test");
        l.opacity = 4.0;
        assert_eq!(l.effective_opacity(), 1.0);
        l.opacity = -1.0;
        assert_eq!(l.effective_opacity(), 0.0);
    }
}
