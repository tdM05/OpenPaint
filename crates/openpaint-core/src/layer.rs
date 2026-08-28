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

/// One layer of a page: how it looks, not what is on it.
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    id: u32,
    pub name: String,
    /// 0..=1, applied to the whole layer at composite time.
    pub opacity: f32,
    pub blend: Blend,
    pub visible: bool,
}

impl Layer {
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
        }
    }

    /// Stable for the layer's whole life, and never reused. Tiles are keyed by it.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.id
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
