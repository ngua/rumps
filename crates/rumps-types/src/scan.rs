//! Range-bounded scan types for cursor-based iteration.

use std::ops::{
    Bound, Range, RangeFrom, RangeFull, RangeInclusive, RangeTo,
    RangeToInclusive,
};

use crate::Key;

/// A pair of bounds describing a key range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bounds(Bound<Key>, Bound<Key>);

impl Bounds {
    /// Creates a new `Bounds` from explicit start and end bounds.
    pub fn new(start: Bound<Key>, end: Bound<Key>) -> Self {
        Self(start, end)
    }

    /// Returns a `Bounds` covering all keys.
    pub fn full() -> Self {
        Self(Bound::Unbounded, Bound::Unbounded)
    }

    /// Returns a reference to the start bound.
    pub fn start(&self) -> &Bound<Key> {
        &self.0
    }

    /// Returns a reference to the end bound.
    pub fn end(&self) -> &Bound<Key> {
        &self.1
    }

    /// Returns `true` if `k` falls within these bounds.
    pub fn contains(&self, k: &Key) -> bool {
        let after_start = match &self.0 {
            Bound::Unbounded => true,
            Bound::Included(lo) => k >= lo,
            Bound::Excluded(lo) => k > lo,
        };
        let before_end = match &self.1 {
            Bound::Unbounded => true,
            Bound::Included(hi) => k <= hi,
            Bound::Excluded(hi) => k < hi,
        };
        after_start && before_end
    }
}

impl From<RangeFull> for Bounds {
    fn from(_: RangeFull) -> Self {
        Self::full()
    }
}

impl From<Range<Key>> for Bounds {
    fn from(r: Range<Key>) -> Self {
        Self(Bound::Included(r.start), Bound::Excluded(r.end))
    }
}

impl From<RangeFrom<Key>> for Bounds {
    fn from(r: RangeFrom<Key>) -> Self {
        Self(Bound::Included(r.start), Bound::Unbounded)
    }
}

impl From<RangeTo<Key>> for Bounds {
    fn from(r: RangeTo<Key>) -> Self {
        Self(Bound::Unbounded, Bound::Excluded(r.end))
    }
}

impl From<RangeInclusive<Key>> for Bounds {
    fn from(r: RangeInclusive<Key>) -> Self {
        let (start, end) = r.into_inner();
        Self(Bound::Included(start), Bound::Included(end))
    }
}

impl From<RangeToInclusive<Key>> for Bounds {
    fn from(r: RangeToInclusive<Key>) -> Self {
        Self(Bound::Unbounded, Bound::Included(r.end))
    }
}

impl From<(Bound<Key>, Bound<Key>)> for Bounds {
    fn from((start, end): (Bound<Key>, Bound<Key>)) -> Self {
        Self(start, end)
    }
}

/// Iteration direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// Ascending key order.
    Forward,
    /// Descending key order.
    Reverse,
}

/// A bounded, directed scan over keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scan {
    /// The key range to scan.
    pub(crate) bounds: Bounds,
    /// The direction to iterate.
    pub(crate) dir: Dir,
}

impl Scan {
    /// Creates a new scan with explicit bounds and direction.
    pub fn new(bounds: impl Into<Bounds>, dir: Dir) -> Self {
        Self {
            bounds: bounds.into(),
            dir,
        }
    }

    /// Returns a reference to the bounds.
    pub fn bounds(&self) -> &Bounds {
        &self.bounds
    }

    /// Returns the scan direction.
    pub fn dir(&self) -> Dir {
        self.dir
    }

    /// Creates a forward scan over the given bounds.
    pub fn forward(bounds: impl Into<Bounds>) -> Self {
        Self::new(bounds, Dir::Forward)
    }

    /// Creates a reverse scan over the given bounds.
    pub fn reverse(bounds: impl Into<Bounds>) -> Self {
        Self::new(bounds, Dir::Reverse)
    }
}

impl Default for Scan {
    fn default() -> Self {
        Self::forward(..)
    }
}

impl From<Option<&Key>> for Scan {
    /// Converts from the legacy `start` parameter.
    ///
    /// `None` produces a full forward scan. `Some(k)` produces a forward
    /// scan starting just after `k` (matching the old `collects` resume
    /// behavior).
    fn from(start: Option<&Key>) -> Self {
        match start {
            None => Self::forward(..),
            Some(k) => Self::forward(Bounds::new(
                Bound::Excluded(k.clone()),
                Bound::Unbounded,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Bound;

    use super::*;
    use crate::key;

    // Bounds conversions

    #[test]
    fn bounds_from_range_full() {
        let b = Bounds::from(..);
        assert_eq!(b.start(), &Bound::Unbounded);
        assert_eq!(b.end(), &Bound::Unbounded);
    }

    #[test]
    fn bounds_from_range() {
        let a = key![1];
        let b_key = key![5];
        let b = Bounds::from(a.clone()..b_key.clone());
        assert_eq!(b.start(), &Bound::Included(a));
        assert_eq!(b.end(), &Bound::Excluded(b_key));
    }

    #[test]
    fn bounds_from_range_inclusive() {
        let a = key![1];
        let b_key = key![5];
        let b = Bounds::from(a.clone()..=b_key.clone());
        assert_eq!(b.start(), &Bound::Included(a));
        assert_eq!(b.end(), &Bound::Included(b_key));
    }

    #[test]
    fn bounds_from_range_from() {
        let a = key![1];
        let b = Bounds::from(a.clone()..);
        assert_eq!(b.start(), &Bound::Included(a));
        assert_eq!(b.end(), &Bound::Unbounded);
    }

    #[test]
    fn bounds_from_range_to() {
        let a = key![5];
        let b = Bounds::from(..a.clone());
        assert_eq!(b.start(), &Bound::Unbounded);
        assert_eq!(b.end(), &Bound::Excluded(a));
    }

    #[test]
    fn bounds_from_range_to_inclusive() {
        let a = key![5];
        let b = Bounds::from(..=a.clone());
        assert_eq!(b.start(), &Bound::Unbounded);
        assert_eq!(b.end(), &Bound::Included(a));
    }

    #[test]
    fn bounds_explicit_excluded() {
        let a = key![1];
        let b = key![5];
        let bounds =
            Bounds::new(Bound::Excluded(a.clone()), Bound::Excluded(b.clone()));
        assert_eq!(bounds.start(), &Bound::Excluded(a));
        assert_eq!(bounds.end(), &Bound::Excluded(b));
    }

    // Bounds::contains

    #[test]
    fn contains_unbounded() {
        let b = Bounds::full();
        assert!(b.contains(&key![0]));
        assert!(b.contains(&key![999]));
    }

    #[test]
    fn contains_included_bounds() {
        let b = Bounds::from(key![2]..=key![5]);
        assert!(!b.contains(&key![1]));
        assert!(b.contains(&key![2]));
        assert!(b.contains(&key![3]));
        assert!(b.contains(&key![5]));
        assert!(!b.contains(&key![6]));
    }

    #[test]
    fn contains_excluded_bounds() {
        let b = Bounds::new(Bound::Excluded(key![2]), Bound::Excluded(key![5]));
        assert!(!b.contains(&key![2]));
        assert!(b.contains(&key![3]));
        assert!(b.contains(&key![4]));
        assert!(!b.contains(&key![5]));
    }

    #[test]
    fn contains_empty_range() {
        let a = key![3];
        let b = Bounds::from(a.clone()..a);
        assert!(!b.contains(&key![2]));
        assert!(!b.contains(&key![3]));
        assert!(!b.contains(&key![4]));
    }

    // Scan::from(Option<&Key>)

    #[test]
    fn scan_from_none() {
        let s = Scan::from(None);
        assert_eq!(s, Scan::forward(..));
        assert_eq!(s.dir, Dir::Forward);
        assert_eq!(s.bounds, Bounds::full());
    }

    #[test]
    fn scan_from_some() {
        let k = key![42];
        let s = Scan::from(Some(&k));
        assert_eq!(s.dir, Dir::Forward);
        assert_eq!(s.bounds.start(), &Bound::Excluded(k));
        assert_eq!(s.bounds.end(), &Bound::Unbounded);
    }
}
