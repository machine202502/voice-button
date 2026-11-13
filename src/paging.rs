//! A window over a SQLite list.
//!
//! The window knows the table's row count and keeps only a few pages around the
//! visible range. Scrolling asks for the pages it needs; nothing ever holds the
//! whole collection. Pages far from the viewport are dropped, so memory stays
//! bounded no matter how long the table grows.
//!
//! There are no page controls in the interface: the list is one endless scroll,
//! and this type is the machinery behind it.

use std::collections::BTreeMap;
use std::ops::Range;

/// Rows fetched per query. Large enough to make scrolling smooth, small enough
/// that a phone with a hundred thousand takes still costs a few hundred rows.
pub const PAGE: usize = 200;

pub struct Paged<T> {
    total: usize,
    pages: BTreeMap<usize, Vec<T>>,
}

impl<T> Default for Paged<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Paged<T> {
    pub fn new() -> Self {
        Self {
            total: 0,
            pages: BTreeMap::new(),
        }
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// Start a new query: forget every page and take the new row count.
    pub fn reset(&mut self, total: usize) {
        self.total = total;
        self.pages.clear();
    }

    pub fn get(&self, index: usize) -> Option<&T> {
        self.pages
            .get(&(index / PAGE))
            .and_then(|page| page.get(index % PAGE))
    }

    /// Rows currently held. Used by the scale test to prove that scrolling a
    /// huge table does not keep the whole table in memory.
    pub fn cached_rows(&self) -> usize {
        self.pages.values().map(Vec::len).sum()
    }

    /// Make sure the pages covering `visible` are loaded. One page on each side
    /// is kept as a scroll margin; everything further away is dropped.
    pub fn ensure<F>(&mut self, visible: Range<usize>, mut load: F)
    where
        F: FnMut(usize, usize) -> Vec<T>,
    {
        if self.total == 0 {
            return;
        }
        let end = visible.end.min(self.total);
        if visible.start >= end {
            return;
        }
        let first = visible.start / PAGE;
        let last = (end - 1) / PAGE;
        for page in first..=last {
            if self.pages.contains_key(&page) {
                continue;
            }
            let offset = page * PAGE;
            let rows = load(offset, PAGE.min(self.total - offset));
            self.pages.insert(page, rows);
        }
        let keep_from = first.saturating_sub(1);
        let keep_to = (last + 1).min(self.total.saturating_sub(1) / PAGE);
        self.pages
            .retain(|page, _| *page >= keep_from && *page <= keep_to);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbers(offset: usize, limit: usize) -> Vec<usize> {
        (offset..offset + limit).collect()
    }

    #[test]
    fn only_pages_near_the_viewport_are_kept() {
        let mut paged = Paged::new();
        paged.reset(100_000);
        paged.ensure(0..30, numbers);
        assert_eq!(paged.get(5), Some(&5));
        assert!(paged.pages.len() <= 3);
        paged.ensure(50_000..50_030, numbers);
        assert_eq!(paged.get(50_010), Some(&50_010));
        assert_eq!(paged.get(5), None, "the old page is gone");
        assert!(paged.pages.len() <= 3);
        assert_eq!(paged.total(), 100_000);
    }

    #[test]
    fn a_short_page_is_returned_whole() {
        let mut paged = Paged::new();
        paged.reset(10);
        paged.ensure(0..10, numbers);
        assert_eq!(paged.get(9), Some(&9));
        assert_eq!(paged.get(10), None);
    }

    #[test]
    fn reset_forgets_the_old_query() {
        let mut paged = Paged::new();
        paged.reset(1000);
        paged.ensure(0..10, numbers);
        assert!(paged.get(0).is_some());
        paged.reset(3);
        assert!(paged.get(0).is_none());
        assert_eq!(paged.total(), 3);
    }
}
