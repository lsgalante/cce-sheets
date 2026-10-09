//! One dimension of a sheet — its columns or its rows — as sizes in logical
//! px: a default size plus sparse runs that differ from it (a custom width, a
//! hidden row). A sheet has 1,048,576 rows, so nothing here walks the indices:
//! `start` is a binary search over the runs and `index_at` a binary search
//! over `start`.
//!
//! Indices are 1-based, as IronCalc's are.

/// A stretch of consecutive indices `start..=end` that all measure `size`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Run {
    start: i32,
    end: i32,
    size: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Axis {
    default: f64,
    last: i32,
    /// Sorted, non-overlapping, each inside `1..=last`.
    runs: Vec<Run>,
    /// `extra[k]`: how much longer than default everything before run `k` is.
    extra: Vec<f64>,
}

impl Axis {
    /// An axis of `last` indices, each `default` long except where `overrides`
    /// (`start`, `end`, `size`) say otherwise. Overrides may arrive unsorted;
    /// where two overlap, the earlier-starting one keeps the overlap.
    pub fn new(default: f64, last: i32, overrides: impl IntoIterator<Item = (i32, i32, f64)>) -> Axis {
        let mut raw: Vec<Run> = overrides
            .into_iter()
            .map(|(s, e, size)| Run { start: s.max(1), end: e.min(last), size: size.max(0.0) })
            .filter(|r| r.start <= r.end && r.size != default)
            .collect();
        raw.sort_by_key(|r| r.start);
        let mut runs: Vec<Run> = Vec::with_capacity(raw.len());
        for mut r in raw {
            if let Some(prev) = runs.last() {
                if r.start <= prev.end {
                    r.start = prev.end + 1;
                    if r.start > r.end {
                        continue;
                    }
                }
            }
            runs.push(r);
        }
        let mut extra = Vec::with_capacity(runs.len());
        let mut acc = 0.0;
        for r in &runs {
            extra.push(acc);
            acc += (r.size - default) * (r.end - r.start + 1) as f64;
        }
        Axis { default, last, runs, extra }
    }

    pub fn last(&self) -> i32 {
        self.last
    }

    /// The size of index `i` (0 when hidden).
    pub fn size(&self, i: i32) -> f64 {
        match self.run_at_or_before(i) {
            Some(k) if i <= self.runs[k].end => self.runs[k].size,
            _ => self.default,
        }
    }

    /// The offset of index `i`'s leading edge from index 1's; `start(1)` is 0
    /// and `start(last + 1)` is the axis's whole length.
    pub fn start(&self, i: i32) -> f64 {
        let i = i.clamp(1, self.last + 1);
        let base = (i - 1) as f64 * self.default;
        // Runs that begin before `i` contribute their extra for the indices
        // they cover below `i`.
        match self.run_before(i) {
            None => base,
            Some(k) => {
                let r = self.runs[k];
                let covered = (r.end.min(i - 1) - r.start + 1) as f64;
                base + self.extra[k] + (r.size - self.default) * covered
            }
        }
    }

    /// The trailing edge of index `i`.
    pub fn end(&self, i: i32) -> f64 {
        self.start(i + 1)
    }

    /// The index whose span holds `pos`, clamped into `1..=last`. A hidden
    /// index spans nothing, so the visible one after it is the answer.
    pub fn index_at(&self, pos: f64) -> i32 {
        if pos <= 0.0 {
            // Skip leading hidden indices so a press at 0 lands on something.
            let mut i = 1;
            while i < self.last && self.size(i) == 0.0 {
                i += 1;
            }
            return i;
        }
        // Largest i with start(i) <= pos.
        let (mut lo, mut hi) = (1, self.last);
        while lo < hi {
            let mid = lo + (hi - lo + 1) / 2;
            if self.start(mid) <= pos {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        lo
    }

    /// The last run starting at or before `i`.
    fn run_at_or_before(&self, i: i32) -> Option<usize> {
        let n = self.runs.partition_point(|r| r.start <= i);
        n.checked_sub(1)
    }

    /// The last run starting strictly before `i`.
    fn run_before(&self, i: i32) -> Option<usize> {
        let n = self.runs.partition_point(|r| r.start < i);
        n.checked_sub(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_axis_is_arithmetic() {
        let a = Axis::new(10.0, 100, []);
        assert_eq!(a.start(1), 0.0);
        assert_eq!(a.start(5), 40.0);
        assert_eq!(a.end(100), 1000.0);
        assert_eq!(a.index_at(0.0), 1);
        assert_eq!(a.index_at(9.99), 1);
        assert_eq!(a.index_at(10.0), 2);
        assert_eq!(a.index_at(5000.0), 100);
    }

    #[test]
    fn runs_shift_everything_after_them() {
        // Index 3 is 30 wide, 6..=7 are hidden.
        let a = Axis::new(10.0, 20, [(6, 7, 0.0), (3, 3, 30.0)]);
        assert_eq!(a.size(3), 30.0);
        assert_eq!(a.size(6), 0.0);
        assert_eq!(a.size(8), 10.0);
        assert_eq!(a.start(3), 20.0);
        assert_eq!(a.start(4), 50.0);
        assert_eq!(a.start(6), 70.0);
        assert_eq!(a.start(7), 70.0);
        assert_eq!(a.start(8), 70.0);
        assert_eq!(a.start(9), 80.0);
        assert_eq!(a.index_at(25.0), 3);
        assert_eq!(a.index_at(49.0), 3);
        // The hidden pair spans nothing: 70 is index 8's.
        assert_eq!(a.index_at(70.0), 8);
    }

    #[test]
    fn a_run_partly_below_i_counts_only_what_it_covers() {
        let a = Axis::new(10.0, 20, [(4, 8, 20.0)]);
        assert_eq!(a.start(4), 30.0);
        assert_eq!(a.start(6), 70.0);
        assert_eq!(a.start(9), 130.0);
        assert_eq!(a.index_at(75.0), 6);
    }

    #[test]
    fn overlaps_are_clipped_and_defaults_dropped() {
        let a = Axis::new(10.0, 20, [(1, 5, 20.0), (3, 8, 5.0), (9, 9, 10.0)]);
        assert_eq!(a.size(3), 20.0);
        assert_eq!(a.size(6), 5.0);
        assert_eq!(a.end(8), 5.0 * 20.0 + 3.0 * 5.0);
    }

    #[test]
    fn leading_hidden_indices_are_skipped_at_zero() {
        let a = Axis::new(10.0, 20, [(1, 2, 0.0)]);
        assert_eq!(a.index_at(0.0), 3);
        assert_eq!(a.index_at(1.0), 3);
    }

    #[test]
    fn a_full_sheet_of_rows_is_cheap() {
        let a = Axis::new(25.0, 1_048_576, (1..1000).map(|r| (r * 7, r * 7, 40.0)));
        let p = a.start(1_000_000);
        assert_eq!(a.index_at(p), 1_000_000);
        assert_eq!(a.index_at(p - 0.5), 999_999);
    }
}
