//! A document: an ordered list of pages, plus the presentation mode.
//!
//! This is the model that yields all three products (`docs/DECISIONS.md` §5, §5a):
//! a **webtoon** is one very tall page, a **sketchbook** is many pages, a **print
//! comic** is many pages plus spread pairing. There are deliberately no variants
//! and no mode-specific code paths — two document types would mean two formats, two
//! renderers, and two sets of bugs.
//!
//! # Mode restricts nothing
//!
//! [`Mode`] exists so the *UI* can hide what a given kind of project doesn't need
//! and pick sensible defaults. It is not consulted by the engine, and every
//! capability stays available underneath it: pages can always be added, a page can
//! always be resized in any direction, and upscaling is always possible. If you
//! find the engine branching on `Mode`, something has gone wrong.

use crate::page::Page;

/// What the UI should present. See the module note: this hides and defaults, it does
/// not restrict.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    /// Discrete pages: navigate page-to-page, spreads make sense, extend is hidden.
    /// Print comics and sketchbooks.
    #[default]
    Pages,
    /// One tall page scrolled continuously, with "Extend ↓" offered. Webtoons.
    Continuous,
}

pub struct Document {
    pages: Vec<Page>,
    active: usize,
    mode: Mode,
    /// Layer ids are unique across the whole document, and never reused within a session.
    ///
    /// Document-wide rather than per page, because the renderer keys tiles by layer id alone:
    /// two pages each starting their layers at 0 would have their pixels collide. Kept
    /// monotonic within a session because undo holds tiles keyed by the ids of *deleted*
    /// layers, and handing one out again would let a restore land on the wrong layer.
    next_layer_id: u32,
}

impl Document {
    /// A document with a single page.
    #[must_use]
    pub fn new(page: Page, mode: Mode) -> Self {
        let next_layer_id = page.highest_layer_id() + 1;
        Self {
            pages: vec![page],
            active: 0,
            mode,
            next_layer_id,
        }
    }

    /// Rebuild a document exactly as it was, for loading.
    ///
    /// Returns `None` with no pages: every accessor here assumes an active page exists, and a
    /// document with none would make each of them fallible for the sake of a state no valid
    /// file contains.
    #[must_use]
    pub fn restored(pages: Vec<Page>, active: usize, mode: Mode) -> Option<Self> {
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
            mode,
            next_layer_id,
        })
    }

    #[must_use]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer::Layer;
    use crate::page::PageRect;

    fn doc() -> Document {
        Document::new(Page::new(800, 1000), Mode::Pages)
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

    /// Pages are addable in continuous mode too: mode hides affordances, it does not
    /// restrict capability (§5a). If this ever fails, the engine has started
    /// branching on mode.
    #[test]
    fn pages_can_be_added_in_continuous_mode() {
        let mut d = Document::new(Page::new(800, 4000), Mode::Continuous);
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
        let mut d = Document::new(Page::new(800, 1000), Mode::Continuous);
        let moved = d.active_mut().extend(crate::page::Side::Bottom, 500);
        assert_eq!(moved, (0, 0));
        assert_eq!(d.active().height(), 1500);
    }

    /// The bug this ownership change fixed: layer ids must be unique across the **document**,
    /// not the page. The renderer keys tiles by id alone, so two pages each starting their
    /// layers at 0 would have their pixels land on top of each other.
    #[test]
    fn layer_ids_are_unique_across_pages() {
        let mut d = Document::new(Page::new(100, 100), Mode::Pages);
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
        let mut d = Document::new(Page::new(100, 100), Mode::Pages);
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
        let mut d = Document::new(Page::new(100, 100), Mode::Pages);
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
        let mut d = Document::new(Page::new(10, 10), Mode::Pages);
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
        let mut d = Document::new(Page::new(10, 10), Mode::Pages);
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
        let mut d = Document::new(Page::new(10, 10), Mode::Pages);
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
