//! Order statistics: the median and the interpolated quantile of a window,
//! and a sorted multiset a sliding window keeps them in (insert and remove
//! by binary search; M-8 replaces it by a weight-balanced tree behind the
//! same interface where windows are long).

/// The values in total order (NaN last).
pub fn sorted(xs: &[f64]) -> Vec<f64> {
    let mut s = xs.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    s
}

/// The `q` quantile of sorted values by linear interpolation between the
/// order statistics at `q·(n−1)`.
pub fn quantile_of_sorted(s: &[f64], q: f64) -> f64 {
    let pos = q * (s.len() as f64 - 1.0);
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    s[lo] + (s[hi] - s[lo]) * (pos - lo as f64)
}

/// The median of sorted values: the middle one, or the mean of the two.
pub fn median_of_sorted(s: &[f64]) -> f64 {
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    }
}

/// A multiset of values kept sorted, for a window's order statistics.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SortedWindow {
    v: Vec<f64>,
}

impl SortedWindow {
    pub fn insert(&mut self, x: f64) {
        let at = self.v.partition_point(|y| y.total_cmp(&x).is_lt());
        self.v.insert(at, x);
    }

    /// Removes one copy of `x` (by bits); `false` when it is not held.
    pub fn remove(&mut self, x: f64) -> bool {
        let at = self.v.partition_point(|y| y.total_cmp(&x).is_lt());
        if self.v.get(at).is_some_and(|y| y.to_bits() == x.to_bits()) {
            self.v.remove(at);
            true
        } else {
            false
        }
    }

    pub fn len(&self) -> usize {
        self.v.len()
    }

    pub fn is_empty(&self) -> bool {
        self.v.is_empty()
    }

    pub fn as_slice(&self) -> &[f64] {
        &self.v
    }
}
