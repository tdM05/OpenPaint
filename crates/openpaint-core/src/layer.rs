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
    /// Nothing may change this layer's pixels at all. See [`Layer::is_locked`].
    pub locked: bool,
    /// What this layer is made of. See [`Content`].
    ///
    /// Private, and reached through [`Layer::content`] / [`Layer::set_content`], because changing it
    /// is not an ordinary field write: a layer whose content becomes [`Content::Text`] stops
    /// accepting paint, and one that becomes [`Content::Raster`] has just had its text discarded.
    /// Both are decisions, and the type should make them look like decisions.
    content: Content,
}

/// Everything about a layer except its name, its id and its pixels.
///
/// **One value rather than six fields to chase.** These are what the Layers panel sets, and the
/// reason they are gathered is undo: recording "this layer's settings were *that*" needs one
/// snapshot and one restore, where a variant per property needed six of each and would have
/// needed a seventh the day a property was added. `Copy`, because it is six machine words and is
/// snapshotted on every change.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub opacity: f32,
    pub blend: Blend,
    pub visible: bool,
    pub lock_alpha: bool,
    pub clip_below: bool,
    pub locked: bool,
}

impl Settings {
    /// Which fields differ between two sets of settings, as a bitmask.
    ///
    /// **For deciding whether two changes are the same change.** Undo coalesces a run of edits to
    /// one layer so that dragging a slider is one step rather than forty -- but "the opacity moved
    /// again" and "and then it was locked" are two decisions, and merging them makes one Ctrl+Z
    /// undo something the artist did not ask it to. Comparing *what changed* tells them apart;
    /// comparing the layer alone does not.
    ///
    /// A mask rather than an enum of properties: it costs nothing, and a field added to `Settings`
    /// that nobody remembers to add here shows up as two edits refusing to coalesce, which is
    /// merely ungainly. The other way round -- a new field silently merging with everything --
    /// would be an undo step quietly eating an edit.
    #[must_use]
    pub fn differences(self, other: Self) -> u8 {
        let mut mask = 0;
        // Bit-exact, deliberately: a slider that lands on the same value twice reports no change,
        // and `set_layer_settings` has already refused that case before it gets here.
        if self.opacity.to_bits() != other.opacity.to_bits() {
            mask |= 1;
        }
        if self.blend != other.blend {
            mask |= 2;
        }
        if self.visible != other.visible {
            mask |= 4;
        }
        if self.lock_alpha != other.lock_alpha {
            mask |= 8;
        }
        if self.clip_below != other.clip_below {
            mask |= 16;
        }
        if self.locked != other.locked {
            mask |= 32;
        }
        mask
    }
}

impl Layer {
    /// This layer's settings, as one value.
    #[must_use]
    pub fn settings(&self) -> Settings {
        Settings {
            opacity: self.opacity,
            blend: self.blend,
            visible: self.visible,
            lock_alpha: self.lock_alpha,
            clip_below: self.clip_below,
            locked: self.locked,
        }
    }

    /// Put settings back onto this layer.
    ///
    /// Every field, deliberately: a partial restore is how undo ends up leaving one property at
    /// the value it had *after* the thing being undone.
    pub fn set_settings(&mut self, settings: Settings) {
        let Settings {
            opacity,
            blend,
            visible,
            lock_alpha,
            clip_below,
            locked,
        } = settings;
        self.opacity = opacity;
        self.blend = blend;
        self.visible = visible;
        self.lock_alpha = lock_alpha;
        self.clip_below = clip_below;
        self.locked = locked;
    }

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
            locked: false,
            content: Content::Raster,
        }
    }

    /// The same layer, locked.
    ///
    /// A builder rather than an eighth positional argument, for the reason [`Layer::restored`]'s
    /// own comment gives: that call already takes three bools in a row, and a fourth would make
    /// every call site a line of `true, false, false, true` that nobody can read correctly.
    #[must_use]
    pub fn with_lock(mut self, locked: bool) -> Self {
        self.locked = locked;
        self
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
            locked: false,
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
        matches!(self.content, Content::Raster) && !self.locked
    }

    /// Whether this layer has been set aside: nothing may change its pixels.
    ///
    /// **Not a painting mode, unlike [`locks_alpha`](Layer::locks_alpha).** Alpha lock is
    /// something an artist switches on in order to work -- paint only where there is paint --
    /// while this is switched on in order *not* to work on something: the sketch under the inks,
    /// the flats under the shading. Inking onto the sketch layer is the commonest mistake in this
    /// medium, and the reason every comparable application has this.
    ///
    /// Not the same as hiding it either. Hiding a layer to protect it takes away the thing you
    /// are drawing over, which is the whole reason it is there.
    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.locked
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

    /// A locked layer takes no paint, whatever else is true of it.
    ///
    /// **Through `accepts_paint`, which is the one question everything that paints asks.** Adding
    /// the check anywhere else -- in the brush, in the fill, in the clear -- would have been
    /// three checks and a fourth one missing, which is exactly how a text layer came to be
    /// paintable through one path and not another.
    #[test]
    fn a_locked_layer_takes_no_paint() {
        let mut layer = Layer::new(1, "Sketch");
        assert!(layer.accepts_paint(), "an ordinary layer refuses paint");
        layer.locked = true;
        assert!(!layer.accepts_paint(), "a locked layer accepted paint");
        assert!(layer.is_locked());

        // And unlocking gives it back: a lock is a state, not a conversion.
        layer.locked = false;
        assert!(layer.accepts_paint());
    }

    /// Locking is not hiding, and not an alpha lock.
    ///
    /// Three switches that sound alike; the panel puts them next to each other, and each one
    /// means something the other two do not. A layer set aside must still be visible -- the whole
    /// reason to lock a sketch is to keep drawing over it.
    #[test]
    fn a_lock_leaves_a_layer_visible_and_its_alpha_alone() {
        let mut layer = Layer::new(1, "Sketch");
        layer.locked = true;
        assert!(layer.visible, "locking hid the layer");
        assert!(
            layer.effective_opacity() > 0.0,
            "locking stopped it drawing"
        );
        assert!(!layer.locks_alpha(), "locking froze its transparency");
    }

    /// Two settings differ in exactly the fields that were changed, and no others.
    ///
    /// **This is what tells one edit from another**, and undo leans on it: a run of changes to the
    /// *same* field is one decision -- a slider dragged across its track -- while a change to a
    /// different field is a second decision that must not be swallowed by the first.
    #[test]
    fn differences_name_the_fields_that_moved() {
        let plain = Layer::new(1, "Ink").settings();
        let opaque_less = Settings {
            opacity: 0.5,
            ..plain
        };
        let locked = Settings {
            locked: true,
            ..plain
        };

        assert_eq!(
            plain.differences(plain),
            0,
            "nothing changed, and it said so"
        );
        assert_ne!(plain.differences(opaque_less), 0);
        assert_ne!(plain.differences(locked), 0);

        // The point: two edits to the same field look the same, and two edits to different fields
        // do not.
        let opaque_least = Settings {
            opacity: 0.25,
            ..plain
        };
        assert_eq!(
            plain.differences(opaque_less),
            opaque_less.differences(opaque_least),
            "two moves of the opacity slider are not the same kind of edit"
        );
        assert_ne!(
            plain.differences(opaque_less),
            plain.differences(locked),
            "changing the opacity and locking the layer read as the same edit"
        );

        // Every field, so one added later without a bit here shows up as its own mask rather than
        // silently sharing another's.
        let each = [
            Settings {
                opacity: 0.5,
                ..plain
            },
            Settings {
                blend: Blend::Multiply,
                ..plain
            },
            Settings {
                visible: false,
                ..plain
            },
            Settings {
                lock_alpha: true,
                ..plain
            },
            Settings {
                clip_below: true,
                ..plain
            },
            Settings {
                locked: true,
                ..plain
            },
        ];
        let mut seen: Vec<u8> = each.iter().map(|s| plain.differences(*s)).collect();
        assert!(
            seen.iter().all(|m| *m != 0),
            "a field moved and nothing noticed"
        );
        let all = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), all, "two fields share a bit: {seen:?}");
    }

    /// Settings go out and come back as one value, every field of them.
    ///
    /// The failure this stops is a partial restore: undo putting five of the six back and leaving
    /// the sixth at the value it had *after* the thing being undone, which nothing on screen
    /// would explain.
    #[test]
    fn settings_survive_a_round_trip_whole() {
        let mut layer = Layer::new(1, "Ink");
        let plain = layer.settings();
        layer.opacity = 0.25;
        layer.blend = Blend::Multiply;
        layer.visible = false;
        layer.lock_alpha = true;
        layer.clip_below = true;
        layer.locked = true;
        let changed = layer.settings();
        assert_ne!(plain, changed);

        layer.set_settings(plain);
        assert_eq!(layer.settings(), plain, "something did not go back");
        assert!(layer.visible && !layer.locked && !layer.lock_alpha && !layer.clip_below);
        assert!((layer.opacity - 1.0).abs() < f32::EPSILON);

        layer.set_settings(changed);
        assert_eq!(layer.settings(), changed, "something did not come forward");
    }

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
