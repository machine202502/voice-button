//! Heights of a lazy list whose rows are not all the same size.
//!
//! A chat message with three lines of text and one with thirty cannot share a
//! row height: forcing them into a grid leaves holes under the short ones and
//! cuts the long ones. This keeps the measured height of every row and the
//! running offset of each, so the list can place row N exactly after row N-1 and
//! still answer "which row starts at this pixel" without touching the ones it
//! has never drawn.
//!
//! Rows are measured after they are drawn. A row that has not been drawn yet is
//! assumed to be `estimate` tall; the assumption only moves the scrollbar a
//! little and is replaced the first time the row becomes visible.

/// Measured heights and the offsets they produce.
pub struct LazyList {
    heights: Vec<f32>,
    /// `offsets[i]` is where row `i` starts; `offsets[len]` is the full height.
    offsets: Vec<f32>,
    spacing: f32,
    estimate: f32,
}

impl Default for LazyList {
    fn default() -> Self {
        Self::new(6.0, 120.0)
    }
}

impl LazyList {
    /// `spacing` is the gap between rows, `estimate` the height assumed for a row
    /// that has not been measured yet.
    pub fn new(spacing: f32, estimate: f32) -> Self {
        let estimate = estimate.max(1.0);
        Self {
            heights: Vec::new(),
            offsets: vec![0.0],
            spacing: spacing.max(0.0),
            estimate,
        }
    }

    /// Forget every measurement: used when the rows behind the list change.
    pub fn reset(&mut self) {
        self.heights.clear();
        self.offsets.clear();
        self.offsets.push(0.0);
    }

    pub fn len(&self) -> usize {
        self.heights.len()
    }

    pub fn is_empty(&self) -> bool {
        self.heights.is_empty()
    }

    /// Make room for `len` rows, assuming the estimate for the new ones.
    pub fn ensure(&mut self, len: usize) {
        if len <= self.heights.len() {
            return;
        }
        let start = self.heights.len();
        self.heights.resize(len, self.estimate);
        let mut offset = self.offsets[start];
        for index in start..len {
            offset += self.heights[index] + self.spacing;
            self.offsets.push(offset);
        }
    }

    /// Record what a row really took. Answers whether anything moved.
    pub fn set(&mut self, index: usize, height: f32) -> bool {
        let height = height.max(1.0);
        if index >= self.heights.len() || (self.heights[index] - height).abs() < 0.5 {
            return false;
        }
        self.heights[index] = height;
        let mut offset = self.offsets[index];
        for next in index..self.heights.len() {
            offset += self.heights[next] + self.spacing;
            self.offsets[next + 1] = offset;
        }
        true
    }

    /// Where row `index` starts, in content coordinates.
    pub fn offset(&self, index: usize) -> f32 {
        self.offsets
            .get(index)
            .copied()
            .unwrap_or_else(|| self.total())
    }

    /// The height of the whole list, including the spacing between rows.
    pub fn total(&self) -> f32 {
        self.offsets.last().copied().unwrap_or(0.0)
    }

    /// The last row that starts at or above `y`.
    pub fn index_at(&self, y: f32) -> usize {
        if self.heights.is_empty() || y <= 0.0 {
            return 0;
        }
        // `offsets` is increasing, so a binary search finds the row directly.
        let mut low = 0;
        let mut high = self.heights.len();
        while low + 1 < high {
            let middle = (low + high) / 2;
            if self.offset(middle) <= y {
                low = middle;
            } else {
                high = middle;
            }
        }
        low
    }
}

#[cfg(test)]
mod tests {
    use super::LazyList;

    #[test]
    fn unmeasured_rows_use_the_estimate() {
        let mut list = LazyList::new(4.0, 100.0);
        list.ensure(3);
        assert_eq!(list.len(), 3);
        assert!((list.total() - 312.0).abs() < 0.01);
        assert!((list.offset(1) - 104.0).abs() < 0.01);
    }

    #[test]
    fn a_measured_row_moves_everything_below_it() {
        let mut list = LazyList::new(4.0, 100.0);
        list.ensure(3);
        assert!(list.set(0, 250.0));
        assert!((list.offset(1) - 254.0).abs() < 0.01);
        assert!((list.offset(2) - 358.0).abs() < 0.01);
        assert!((list.total() - 462.0).abs() < 0.01);
    }

    #[test]
    fn measuring_twice_is_free() {
        let mut list = LazyList::new(4.0, 100.0);
        list.ensure(2);
        assert!(list.set(0, 80.0));
        assert!(!list.set(0, 80.2), "half a pixel is not a change");
    }

    #[test]
    fn index_at_finds_the_row_under_the_pointer() {
        let mut list = LazyList::new(0.0, 50.0);
        list.ensure(4);
        list.set(0, 10.0);
        list.set(1, 200.0);
        // Offsets: 0, 10, 210, 260, 310.
        assert_eq!(list.index_at(0.0), 0);
        assert_eq!(list.index_at(9.0), 0);
        assert_eq!(list.index_at(10.0), 1);
        assert_eq!(list.index_at(209.0), 1);
        assert_eq!(list.index_at(210.0), 2);
        assert_eq!(list.index_at(1_000.0), 3, "past the end stays on the last row");
    }

    #[test]
    fn growing_keeps_the_old_measurements() {
        let mut list = LazyList::new(0.0, 40.0);
        list.ensure(2);
        list.set(1, 300.0);
        list.ensure(4);
        assert_eq!(list.len(), 4);
        assert!((list.offset(2) - 340.0).abs() < 0.01);
        assert!((list.total() - 420.0).abs() < 0.01);
    }

    #[test]
    fn reset_forgets_everything() {
        let mut list = LazyList::new(2.0, 30.0);
        list.ensure(3);
        list.set(0, 500.0);
        list.reset();
        assert!(list.is_empty());
        assert_eq!(list.total(), 0.0);
    }
}
