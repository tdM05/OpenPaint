//! A document: an ordered list of pages.
//!
//! This is the model that yields all three products (`docs/DECISIONS.md` §5, §5a):
//! a **webtoon** is one very tall page, a **sketchbook** is many pages, a **print
//! comic** is many pages plus spread pairing. There are deliberately no variants
//! and no mode-specific code paths — two document types would mean two formats, two
//! renderers, and two sets of bugs.
//!
//! # There is no document type, and no mode either
//!
//! An earlier version carried a `Mode` (Pages / Continuous) so the UI could hide what a given
//! kind of project did not need. It was removed once it became clear nothing could consult it:
//! every affordance it was meant to hide is unconditionally available by other decisions -- a
//! page can always be extended in any direction, pages can always be added -- and "continuous
//! scrolling" has nothing to scroll across once a webtoon is *one very tall page*
//! (`docs/DECISIONS.md` §5a). A flag no code reads is worse than no flag, because it implies
//! behaviour that does not exist.
//!
//! What the idea was really reaching for lives elsewhere: **new-document presets** (a strip
//! versus A4 at 300 DPI) are a creation-time choice, and **strip slicing** is an export option
//! (§7). Neither is a lasting property of a document.

use crate::page::Page;

pub struct Document {
    pages: Vec<Page>,
    active: usize,
    /// Layer ids are unique across the whole document, and never reused within a session.
    ///
    /// Document-wide rather than per page, because the renderer keys tiles by layer id alone:
    /// two pages each starting their layers at 0 would have their pixels collide. Kept
    /// monotonic within a session because undo holds tiles keyed by the ids of *deleted*
    /// layers, and handing one out again would let a restore land on the wrong layer.
    next_layer_id: u32,
    /// Colours saved with this document, in the order they were added.
    ///
    /// **Document content, not an app setting**, and that is the decision worth naming. A comic's
    /// palette is a property of the comic: the skin tone that has to match on page forty is the
    /// same one from page one, and it has to survive being handed to a colourist on another
    /// machine. A per-user swatch list is a different, smaller feature — "colours I like" rather
    /// than "colours this story uses" — and it can arrive later without this being in its way.
    ///
    /// Authored sRGB, not linear premultiplied. A palette entry is a colour the artist *chose*, so
    /// it is stored the way they chose it: converting to linear and back is lossy at 8 bits, and a
    /// swatch that drifted a shade each time the file was opened would be worse than useless.
    palette: Vec<[u8; 3]>,
}

impl Document {
    /// A document with a single page.
    #[must_use]
    pub fn new(page: Page) -> Self {
        let next_layer_id = page.highest_layer_id() + 1;
        Self {
            pages: vec![page],
            active: 0,
            next_layer_id,
            palette: Vec::new(),
        }
    }

    /// Rebuild a document exactly as it was, for loading.
    ///
    /// Returns `None` with no pages: every accessor here assumes an active page exists, and a
    /// document with none would make each of them fallible for the sake of a state no valid
    /// file contains.
    #[must_use]
    pub fn restored(pages: Vec<Page>, active: usize) -> Option<Self> {
        if pages.is_empty() {
            return None;
        }
        // Derived rather than stored. A save discards undo history, which is the only thing
        // that made reusing a dead layer's id dangerous -- so one past the highest live id is
        // exactly right on load, and needs nothing from the file to be correct.
        let next_layer_id = pages.iter().map(Page::highest_layer_id).max().unwrap_or(0) + 1;
        Some(Self {
            active: active.min(pages.len() - 1),
            pages,
            next_layer_id,
            // Set afterwards by the loader rather than taken here: a file written before palettes
            // existed simply has none, and an empty one is the honest answer for it.
            palette: Vec::new(),
        })
    }

    #[must_use]
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Index of the page being edited.
    #[must_use]
    pub fn active_index(&self) -> usize {
        self.active
    }

    /// The page being edited.
    ///
    /// Infallible by construction: a document always has at least one page, and
    /// `active` is clamped whenever pages change. That is deliberate — an editor
    /// with no page to draw on is not a state worth representing.
    #[must_use]
    pub fn active(&self) -> &Page {
        &self.pages[self.active]
    }

    pub fn active_mut(&mut self) -> &mut Page {
        &mut self.pages[self.active]
    }

    pub fn page_mut(&mut self, index: usize) -> Option<&mut Page> {
        self.pages.get_mut(index)
    }

    #[must_use]
    pub fn page(&self, index: usize) -> Option<&Page> {
        self.pages.get(index)
    }

    /// Switch pages. Out-of-range indices are ignored rather than clamped, so a
    /// stale index from the UI can't silently move the user somewhere unexpected.
    pub fn set_active(&mut self, index: usize) -> bool {
        if index < self.pages.len() {
            self.active = index;
            true
        } else {
            false
        }
    }

    /// Append a page and make it active. Available in every mode (§5a).
    pub fn add_page(&mut self, page: Page) -> usize {
        self.next_layer_id = self.next_layer_id.max(page.highest_layer_id() + 1);
        self.pages.push(page);
        self.active = self.pages.len() - 1;
        self.active
    }

    /// Add an empty page the same size as the active one, directly after it, and select it.
    ///
    /// After the active page rather than at the end, for the same reason a new layer goes above
    /// the active one: the page you just made is nearly always meant to follow the one you were
    /// working on.
    pub fn add_page_like_active(&mut self) -> usize {
        let rect = self.active().rect();
        let dpi = self.active().dpi();
        let id = self.take_layer_id();
        let mut page = Page::with_layer_id(rect.w, rect.h, id);
        page.set_dpi(dpi);
        // Keep the new page's rectangle where the old one's was, so a webtoon strip extended
        // upward does not put its next page somewhere else entirely.
        page.resize(rect);
        self.insert_page(self.active + 1, page)
    }

    /// The colours saved with this document.
    #[must_use]
    pub fn palette(&self) -> &[[u8; 3]] {
        &self.palette
    }

    /// Replace the whole palette, for loading.
    pub fn set_palette(&mut self, palette: Vec<[u8; 3]>) {
        self.palette = palette;
    }

    /// Add a colour, unless it is already there. Returns whether it was added.
    ///
    /// Refusing a duplicate rather than allowing one is what makes the swatch row usable: a
    /// palette that fills up with six copies of the same black is a palette nobody can pick from.
    /// Exact equality is the right test because these are authored values — the artist either
    /// chose this colour before or they did not.
    pub fn add_to_palette(&mut self, rgb: [u8; 3]) -> bool {
        if self.palette.contains(&rgb) {
            return false;
        }
        self.palette.push(rgb);
        true
    }

    /// Remove the colour at an index. Returns whether there was one.
    pub fn remove_from_palette(&mut self, index: usize) -> bool {
        if index >= self.palette.len() {
            return false;
        }
        self.palette.remove(index);
        true
    }

    /// Take the next layer id. The only place ids come from.
    fn take_layer_id(&mut self) -> u32 {
        let id = self.next_layer_id;
        self.next_layer_id += 1;
        id
    }

    /// Add a layer above the active one of the active page, and select it.
    pub fn add_layer(&mut self) -> usize {
        let id = self.take_layer_id();
        // Named from the id, not the count: naming from the count reuses names as soon as a
        // layer is deleted, and two layers called "Layer 2" is worse than a gap in the
        // numbering.
        let name = format!("Layer {}", id + 1);
        let at = self.active().active_index();
        self.active_mut().insert_layer_above(at, id, name)
    }

    /// Copy a layer's *properties* above it, and select the copy. Returns its index, and the id
    /// of the new layer so the caller can copy the pixels into it.
    ///
    /// Properties only, because pixels are not the document's to move: they live on the GPU, and
    /// the whole point of the split is that this module never learns about tiles. The caller
    /// copies them, which it is in a position to do and this is not.
    ///
    /// Text comes along as text, so duplicating a caption gives a second editable caption rather
    /// than a picture of the first. That falls out of the content being the layer's source of
    /// truth (§6a) rather than needing a case of its own.
    pub fn duplicate_layer(&mut self, index: usize) -> Option<(usize, u32)> {
        let source = self.active().layer(index)?.clone();
        let id = self.take_layer_id();
        let at = self
            .active_mut()
            .insert_layer_above(index, id, copy_name(&source.name));
        let copy = self
            .active_mut()
            .layer_mut(at)
            .expect("the layer just inserted");
        copy.opacity = source.opacity;
        copy.blend = source.blend;
        copy.visible = source.visible;
        copy.lock_alpha = source.lock_alpha;
        copy.clip_below = source.clip_below;
        copy.set_content(source.content().clone());
        Some((at, id))
    }

    /// Add a text layer above the active one, and select it. Returns its index.
    ///
    /// Named from its own text rather than from the id, unlike [`Document::add_layer`]: a caption
    /// has something to be called, and "Layer 8" would throw it away.
    pub fn add_text_layer(&mut self, block: crate::text::TextBlock) -> usize {
        let id = self.take_layer_id();
        let at = self.active().active_index();
        self.active_mut().insert_text_layer_above(at, id, block)
    }

    /// Move a page to a new index, shifting the rest. Returns where it ended up.
    pub fn move_page(&mut self, from: usize, to: usize) -> Option<usize> {
        if from >= self.pages.len() {
            return None;
        }
        let to = to.min(self.pages.len() - 1);
        if from == to {
            return Some(to);
        }
        let page = self.pages.remove(from);
        self.pages.insert(to, page);
        // Follow the page that moved, if it was the active one.
        if self.active == from {
            self.active = to;
        } else if from < self.active && to >= self.active {
            self.active -= 1;
        } else if from > self.active && to <= self.active {
            self.active += 1;
        }
        Some(to)
    }

    /// Put a removed page back at `index`, keeping its layer ids.
    ///
    /// For undoing a deletion, which is why it takes a whole [`Page`]: the ids have to be the
    /// same or the tiles history is about to restore would belong to nothing.
    pub fn restore_page(&mut self, index: usize, page: Page) -> usize {
        self.next_layer_id = self.next_layer_id.max(page.highest_layer_id() + 1);
        let at = index.min(self.pages.len());
        self.pages.insert(at, page);
        self.active = at;
        at
    }

    /// Insert a page at `index`, making it active.
    pub fn insert_page(&mut self, index: usize, page: Page) -> usize {
        let index = index.min(self.pages.len());
        self.pages.insert(index, page);
        self.active = index;
        index
    }

    /// Remove a page. Refuses to remove the last one — a document must always have
    /// somewhere to draw.
    pub fn remove_page(&mut self, index: usize) -> Option<Page> {
        if self.pages.len() <= 1 || index >= self.pages.len() {
            return None;
        }
        let page = self.pages.remove(index);
        self.active = self.active.min(self.pages.len() - 1);
        Some(page)
    }
}

/// What a duplicate is called: "Ink" becomes "Ink copy", and "Ink copy" stays "Ink copy 2".
///
/// Numbered on the second copy rather than the first, which is what every app does and reads
/// better than "Ink copy copy".
fn copy_name(name: &str) -> String {
    let Some(stem) = name.strip_suffix(" copy") else {
        return match name.rsplit_once(" copy ") {
            Some((stem, n)) => match n.parse::<u32>() {
                Ok(n) => format!("{stem} copy {}", n + 1),
                Err(_) => format!("{name} copy"),
            },
            None => format!("{name} copy"),
        };
    };
    format!("{stem} copy 2")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A palette refuses duplicates, because a row with six copies of the same black is one
    /// nobody can pick from.
    #[test]
    fn a_palette_keeps_each_colour_once_and_in_order() {
        let mut doc = Document::new(Page::new(256, 256));
        assert!(doc.add_to_palette([10, 20, 30]));
        assert!(doc.add_to_palette([200, 0, 0]));
        assert!(
            !doc.add_to_palette([10, 20, 30]),
            "a colour already kept is not kept twice"
        );
        assert_eq!(doc.palette(), [[10, 20, 30], [200, 0, 0]]);

        assert!(doc.add_to_palette([1, 2, 3]));
        // Deliberately not index 0: removing the first entry is the one case where "remove the
        // index asked for" and "remove the first" agree, so it cannot tell them apart.
        assert!(doc.remove_from_palette(1));
        assert_eq!(
            doc.palette(),
            [[10, 20, 30], [1, 2, 3]],
            "the middle one went, and only it"
        );
        assert!(
            !doc.remove_from_palette(5),
            "an index past the end removes nothing"
        );
    }

    /// A duplicate carries everything that makes the layer look the way it does, and its content
    /// with it — so duplicating a caption gives a second editable caption, not a picture of one.
    #[test]
    fn a_duplicate_is_the_same_layer_with_a_new_name() {
        let mut doc = Document::new(Page::new(256, 256));
        {
            let layer = doc.active_mut().layer_mut(0).expect("the first layer");
            layer.name = "Ink".to_owned();
            layer.opacity = 0.4;
            layer.blend = crate::Blend::Multiply;
            layer.clip_below = true;
            layer.lock_alpha = true;
            layer.visible = false;
        }
        let (at, id) = doc.duplicate_layer(0).expect("the layer exists");
        let source = doc.active().layer(0).expect("still there").clone();
        let copy = doc.active().layer(at).expect("the copy");

        assert_eq!(at, 1, "the copy sits directly above its source");
        assert_ne!(
            id,
            source.id(),
            "a copy is its own layer, not a second name for one"
        );
        assert_eq!(copy.name, "Ink copy");
        assert_eq!(copy.opacity, source.opacity);
        assert_eq!(copy.blend, source.blend);
        assert_eq!(copy.clip_below, source.clip_below);
        assert_eq!(copy.lock_alpha, source.lock_alpha);
        assert_eq!(copy.visible, source.visible);
    }

    /// Copies of copies number rather than stacking the word.
    #[test]
    fn copy_names_number_instead_of_repeating() {
        assert_eq!(copy_name("Ink"), "Ink copy");
        assert_eq!(copy_name("Ink copy"), "Ink copy 2");
        assert_eq!(copy_name("Ink copy 2"), "Ink copy 3");
        // Not a number: treat it as an ordinary name rather than guessing.
        assert_eq!(copy_name("Ink copy of mine"), "Ink copy of mine copy");
    }

    use crate::layer::Layer;
    use crate::page::PageRect;

    fn doc() -> Document {
        Document::new(Page::new(800, 1000))
    }

    #[test]
    fn a_new_document_has_one_active_page() {
        let d = doc();
        assert_eq!(d.page_count(), 1);
        assert_eq!(d.active_index(), 0);
        assert_eq!((d.active().width(), d.active().height()), (800, 1000));
    }

    #[test]
    fn adding_a_page_makes_it_active() {
        let mut d = doc();
        let i = d.add_page(Page::new(400, 400));
        assert_eq!(i, 1);
        assert_eq!(d.page_count(), 2);
        assert_eq!(d.active_index(), 1);
        assert_eq!(d.active().width(), 400);
    }

    #[test]
    fn pages_can_be_added_in_continuous_mode() {
        let mut d = Document::new(Page::new(800, 4000));
        d.add_page(Page::new(800, 4000));
        assert_eq!(d.page_count(), 2);
    }

    #[test]
    fn inserting_places_the_page_and_activates_it() {
        let mut d = doc();
        d.add_page(Page::new(100, 100));
        let i = d.insert_page(1, Page::new(222, 222));
        assert_eq!(i, 1);
        assert_eq!(d.page_count(), 3);
        assert_eq!(d.active().width(), 222);
        assert_eq!(d.page(2).map(Page::width), Some(100));
    }

    #[test]
    fn inserting_past_the_end_appends() {
        let mut d = doc();
        let i = d.insert_page(99, Page::new(50, 50));
        assert_eq!(i, 1);
        assert_eq!(d.page_count(), 2);
    }

    /// A document must always have a page to draw on, so the last one can't go.
    #[test]
    fn the_last_page_cannot_be_removed() {
        let mut d = doc();
        assert!(d.remove_page(0).is_none());
        assert_eq!(d.page_count(), 1);
    }

    #[test]
    fn removing_keeps_the_active_index_valid() {
        let mut d = doc();
        d.add_page(Page::new(1, 1));
        d.add_page(Page::new(2, 2));
        assert_eq!(d.active_index(), 2);

        assert!(d.remove_page(2).is_some());
        assert_eq!(d.page_count(), 2);
        assert!(
            d.active_index() < d.page_count(),
            "active index left dangling at {}",
            d.active_index()
        );
    }

    #[test]
    fn switching_to_a_bad_index_is_refused_not_clamped() {
        let mut d = doc();
        assert!(!d.set_active(7));
        assert_eq!(d.active_index(), 0, "a stale index must not move the user");
    }

    #[test]
    fn resizing_the_active_page_reports_the_shift() {
        let mut d = Document::new(Page::new(800, 1000));
        let moved = d.active_mut().extend(crate::page::Side::Bottom, 500);
        assert_eq!(moved, (0, 0));
        assert_eq!(d.active().height(), 1500);
    }

    /// The bug this ownership change fixed: layer ids must be unique across the **document**,
    /// not the page. The renderer keys tiles by id alone, so two pages each starting their
    /// layers at 0 would have their pixels land on top of each other.
    #[test]
    fn layer_ids_are_unique_across_pages() {
        let mut d = Document::new(Page::new(100, 100));
        d.add_layer();
        d.add_page_like_active();
        d.add_layer();
        d.add_page_like_active();

        let mut ids: Vec<u32> = (0..d.page_count())
            .flat_map(|i| {
                d.page(i)
                    .expect("page")
                    .layers()
                    .iter()
                    .map(Layer::id)
                    .collect::<Vec<_>>()
            })
            .collect();
        let total = ids.len();
        assert!(total >= 5, "expected several layers, got {total}");
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), total, "an id was shared between pages");
    }

    /// A new page follows the active one rather than going to the end, for the same reason a new
    /// layer goes above the active one.
    #[test]
    fn a_new_page_follows_the_active_one() {
        let mut d = Document::new(Page::new(100, 100));
        d.add_page_like_active();
        d.add_page_like_active();
        assert_eq!(d.page_count(), 3);
        assert!(d.set_active(0));
        assert_eq!(d.add_page_like_active(), 1);
        assert_eq!(d.active_index(), 1, "the new page should be selected");
        assert_eq!(d.page_count(), 4);
    }

    /// A new page inherits the active one's size and DPI, so a sketchbook stays uniform and a
    /// webtoon's next page matches the strip.
    #[test]
    fn a_new_page_matches_the_one_it_follows() {
        let mut d = Document::new(Page::new(100, 100));
        d.active_mut().set_dpi(300.0);
        d.active_mut().resize(PageRect::new(-40, -70, 800, 1200));
        d.add_page_like_active();
        assert_eq!(d.active().rect(), PageRect::new(-40, -70, 800, 1200));
        assert!((d.active().dpi() - 300.0).abs() < 1e-3);
    }

    /// Reordering has to carry the selection with the page that moved, or the artist keeps
    /// drawing on a different page than the one they dragged.
    #[test]
    fn reordering_pages_follows_the_selection() {
        let mut d = Document::new(Page::new(10, 10));
        d.add_page_like_active();
        d.add_page_like_active();
        assert!(d.set_active(0));
        let selected = d.active().active_layer().id();
        assert_eq!(d.move_page(0, 2), Some(2));
        assert_eq!(d.active_index(), 2, "selection did not follow");
        assert_eq!(d.active().active_layer().id(), selected);

        // Moving another page past the selection keeps it pointing at the same page.
        d.move_page(0, 2);
        assert_eq!(d.active().active_layer().id(), selected);
    }

    /// Deleting must hand the page back, or its tiles would be stranded with nothing to restore
    /// them -- and page deletion has to be undoable for the same reason layer deletion does.
    #[test]
    fn removing_a_page_hands_it_back() {
        let mut d = Document::new(Page::new(10, 10));
        d.add_page_like_active();
        let doomed = d.active().active_layer().id();
        let page = d.remove_page(1).expect("removable");
        assert_eq!(page.active_layer().id(), doomed);
        assert_eq!(d.page_count(), 1);

        // And putting it back keeps its ids, so its tiles still belong to it.
        d.restore_page(1, page);
        assert_eq!(d.page_count(), 2);
        assert_eq!(d.page(1).expect("page").active_layer().id(), doomed);
    }

    /// Restoring a page must not let a later `add_layer` reuse one of its ids.
    #[test]
    fn restoring_a_page_keeps_the_counter_ahead() {
        let mut d = Document::new(Page::new(10, 10));
        d.add_page_like_active();
        d.add_layer();
        let page = d.remove_page(1).expect("removable");
        let ids: Vec<u32> = page.layers().iter().map(Layer::id).collect();
        d.restore_page(1, page);
        d.add_layer();
        let fresh = d.active().active_layer().id();
        assert!(!ids.contains(&fresh), "id {fresh} collides with {ids:?}");
    }
}
