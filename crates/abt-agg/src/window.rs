//! Window states (P-1.2): what both layers keep per group and window as rows
//! enter and leave in canonical order, and the value they give at each bar.
//!
//! [`WindowState`] is the sliding window of a time extent (`window(δ)`,
//! `bucket(C)`): rows enter with `push`, leave with `evict`, and the value is
//! the reduction of the rows held. [`RowsWindow`] is a `rows(N)` window: the
//! rows among the last N anchored labels of the window's calendar, with the
//! running sums of `numerics running` brought along the labels exactly as the
//! kernel brings them (rebuilt from the window's rows at every
//! `max(N, 8)`-th label, advanced at every anchored label), so that a
//! data-layer pass and the kernel give the same bits (Proposition P-1.1).

use std::collections::VecDeque;

use crate::function::{AggError, Function, Mode};
use crate::moments::{checkpoint_period, ShiftedSums};
use crate::reduce::reduce;

/// A sliding window over a time extent: the rows held, in time order.
#[derive(Clone, Debug)]
pub struct WindowState {
    f: Function,
    mode: Mode,
    rows: VecDeque<(i64, [f64; 2])>,
}

impl WindowState {
    pub fn new(f: Function, mode: Mode) -> WindowState {
        WindowState { f, mode, rows: VecDeque::new() }
    }

    /// A row at time `t` enters (rows enter in time order).
    pub fn push(&mut self, t: i64, x: &[f64]) {
        debug_assert!(self.rows.back().is_none_or(|(u, _)| *u <= t), "rows enter in time order");
        self.rows.push_back((t, [x.first().copied().unwrap_or(0.0), x.get(1).copied().unwrap_or(0.0)]));
    }

    /// Every row before `t` leaves.
    pub fn evict(&mut self, t: i64) {
        while self.rows.front().is_some_and(|(u, _)| *u < t) {
            self.rows.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The function over the rows held; `Ok(None)` below the minimum count
    /// the caller asks for, or on an empty window.
    pub fn value(&self, min: usize) -> Result<Option<f64>, AggError> {
        if self.rows.len() < min {
            return Ok(None);
        }
        let a: Vec<f64> = self.rows.iter().map(|r| r.1[0]).collect();
        let b: Vec<f64> = self.rows.iter().map(|r| r.1[1]).collect();
        let cols: Vec<&[f64]> = if self.f.arity() == 2 { vec![&a, &b] } else { vec![&a] };
        reduce(self.f, self.mode, &cols)
    }
}

/// Anchored labels a rows window keeps beyond its N and its checkpoint span,
/// as the kernel does (a request a few labels back is served).
pub const ROWS_SLACK: usize = 16;

/// A `rows(N)` window walked label by label.
#[derive(Clone, Debug)]
pub struct RowsWindow {
    f: Function,
    mode: Mode,
    n: usize,
    /// The window reaches the group's first label.
    from_start: bool,
    anchors: VecDeque<i64>,
    rows: VecDeque<(i64, [f64; 2])>,
    /// The id of `rows[0]`: a row's id is its position since the walk began.
    base_id: u64,
    /// The running sums with the row ids `lo..hi` they hold.
    sums: Option<(u64, u64, ShiftedSums)>,
    at: i64,
}

impl RowsWindow {
    /// A window of `n` anchored labels walked from the group's first label.
    pub fn new(f: Function, mode: Mode, n: usize) -> RowsWindow {
        RowsWindow {
            f,
            mode,
            n,
            from_start: true,
            anchors: VecDeque::new(),
            rows: VecDeque::new(),
            base_id: 0,
            sums: None,
            at: i64::MIN,
        }
    }

    /// The window's rows at label `t`: positions `lo..hi` in the buffer.
    fn window_at(&self, t: i64) -> Option<(usize, usize)> {
        let hi_a = self.anchors.partition_point(|a| *a <= t);
        if hi_a == 0 {
            return Some((self.rows.len(), self.rows.len()));
        }
        let lo_t = if hi_a >= self.n && self.n > 0 {
            self.anchors[hi_a - self.n]
        } else if self.from_start {
            self.anchors[0]
        } else {
            return None;
        };
        let lo = self.rows.partition_point(|(u, _)| *u < lo_t);
        let hi = self.rows.partition_point(|(u, _)| *u <= t);
        Some((lo, hi))
    }

    fn rebuild(&mut self, lo: usize, hi: usize) {
        let sums = ShiftedSums::of_rows(self.rows.range(lo..hi).map(|r| r.1.as_slice()));
        self.sums = Some((self.base_id + lo as u64, self.base_id + hi as u64, sums));
    }

    /// The label with index `index` in the window's calendar: whether it is
    /// anchored for the group, and the group's row there if it has one.
    /// Labels are stepped in order, every label of the calendar.
    pub fn step(&mut self, index: u64, t: i64, anchored: bool, row: Option<&[f64]>) {
        if anchored {
            self.anchors.push_back(t);
        }
        if let Some(x) = row {
            self.rows.push_back((t, [x.first().copied().unwrap_or(0.0), x.get(1).copied().unwrap_or(0.0)]));
        }
        self.at = t;
        if self.mode == Mode::Running && self.f.running() {
            let k = checkpoint_period(self.n) as u64;
            if let Some((lo, hi)) = self.window_at(t) {
                let base = self.base_id;
                let (lo_id, hi_id) = (base + lo as u64, base + hi as u64);
                match self.sums.as_mut() {
                    Some(_) if index.is_multiple_of(k) => self.rebuild(lo, hi),
                    None => self.rebuild(lo, hi),
                    Some((slo, shi, sums)) if anchored => {
                        for id in (*shi).max(lo_id)..hi_id {
                            sums.apply(&self.rows[(id - base) as usize].1, 1.0);
                        }
                        for id in *slo..lo_id.min(*shi) {
                            sums.apply(&self.rows[(id - base) as usize].1, -1.0);
                        }
                        *slo = lo_id.max(*slo);
                        *shi = hi_id.max(*shi);
                    }
                    Some(_) => {}
                }
            }
        }
        // Keep N, a checkpoint's span and the slack.
        while self.anchors.len() > self.n + checkpoint_period(self.n) + ROWS_SLACK {
            self.anchors.pop_front();
            self.from_start = false;
        }
        let oldest = self.anchors.front().copied().unwrap_or(i64::MAX);
        while self.rows.front().is_some_and(|(u, _)| *u < oldest) {
            self.rows.pop_front();
            self.base_id += 1;
        }
    }

    /// The rows of the window at the last label stepped.
    pub fn count(&self) -> usize {
        self.window_at(self.at).map(|(lo, hi)| hi - lo).unwrap_or(0)
    }

    /// The function at the last label stepped, at least `min` rows: the
    /// running sums where they answer, else the two-pass reduction of the
    /// window's rows in time order.
    pub fn value(&mut self, min: usize) -> Result<Option<f64>, AggError> {
        let Some((lo, hi)) = self.window_at(self.at) else { return Ok(None) };
        let count = hi - lo;
        if count < min || count == 0 && self.f != Function::Count {
            return Ok(if self.f == Function::Count && count >= min { Some(0.0) } else { None });
        }
        if self.mode == Mode::Running && self.f.running() && count >= 2 {
            let (lo_id, hi_id) = (self.base_id + lo as u64, self.base_id + hi as u64);
            if !matches!(self.sums, Some((l, h, _)) if l == lo_id && h == hi_id) {
                // A label the walk did not account for: built at `t` itself,
                // as the kernel does.
                self.rebuild(lo, hi);
            }
            if let Some(v) = self.sums.as_ref().and_then(|(_, _, s)| s.value(self.f, count)) {
                return Ok(Some(v));
            }
        }
        let a: Vec<f64> = self.rows.range(lo..hi).map(|r| r.1[0]).collect();
        let b: Vec<f64> = self.rows.range(lo..hi).map(|r| r.1[1]).collect();
        let cols: Vec<&[f64]> = if self.f.arity() == 2 { vec![&a, &b] } else { vec![&a] };
        reduce(self.f, Mode::TwoPass, &cols)
    }
}
