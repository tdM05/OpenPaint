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
}

impl Document {
    /// A document with a single page.
    #[must_use]
    pub fn new(page: Page, mode: Mode) -> Self {
        Self {
            pages: vec![page],
            active: 0,
            mode,
        }
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
        self.pages.push(page);
        self.active = self.pages.len() - 1;
        self.active
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
    pub fn remove_page(&mut self, index: usize) -> bool {
        if self.pages.len() <= 1 || index >= self.pages.len() {
            return false;
        }
        self.pages.remove(index);
        self.active = self.active.min(self.pages.len() - 1);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(!d.remove_page(0));
        assert_eq!(d.page_count(), 1);
    }

    #[test]
    fn removing_keeps_the_active_index_valid() {
        let mut d = doc();
        d.add_page(Page::new(1, 1));
        d.add_page(Page::new(2, 2));
        assert_eq!(d.active_index(), 2);

        assert!(d.remove_page(2));
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
}
