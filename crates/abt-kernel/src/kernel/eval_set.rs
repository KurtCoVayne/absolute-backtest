//! The set-at-a-time evaluator's bindings (M-8; docs/spec/06-performance.md,
//! 3.2). A rule's body is solved over a block of bindings rather than one
//! environment at a time (`Kernel::solve_kleene_block`): each literal maps
//! the block of the bindings entering it to the block of those leaving it,
//! every row a slice of one buffer, with no allocation per instance. The
//! rows leave each literal in the order they enter it, each row's solutions
//! in the order the tuple evaluator yields them, so the two evaluators
//! derive the same tuples in the same order and leave the same instances
//! stuck (Kleene conjunction, V-5). `tests/eval_set.rs` holds them equal on
//! every corpus program and both books.

use super::{Env, Value};

/// Where a literal's solutions go: the tuple evaluator's environments, or
/// a block.
pub(crate) trait Sink {
    /// A copy of `env`.
    fn push_env(&mut self, env: &[Option<Value>]);
    /// A copy of `env` with `slot` set to `v`.
    fn push_with(&mut self, env: &[Option<Value>], slot: usize, v: Option<Value>);
    /// An environment already built.
    fn push_owned(&mut self, env: Env);
    /// The last solution pushed, to bind more slots in place.
    fn last_row(&mut self) -> Option<&mut [Option<Value>]>;
}

impl Sink for Vec<Env> {
    fn push_env(&mut self, env: &[Option<Value>]) {
        self.push(env.to_vec());
    }
    fn push_with(&mut self, env: &[Option<Value>], slot: usize, v: Option<Value>) {
        let mut e = env.to_vec();
        e[slot] = v;
        self.push(e);
    }
    fn push_owned(&mut self, env: Env) {
        self.push(env);
    }
    fn last_row(&mut self) -> Option<&mut [Option<Value>]> {
        self.last_mut().map(|e| e.as_mut_slice())
    }
}

/// A block of bindings: `rows` rows of `stride` slots each, row-major in
/// one buffer.
#[derive(Clone, Debug, Default)]
pub(crate) struct Block {
    stride: usize,
    rows: usize,
    cells: Vec<Option<Value>>,
}

impl Block {
    pub fn with_capacity(stride: usize, rows: usize) -> Block {
        Block {
            stride,
            rows: 0,
            cells: Vec::with_capacity(stride * rows),
        }
    }

    /// A block of the environments given, in their order.
    pub fn of(stride: usize, envs: Vec<Env>) -> Block {
        let mut b = Block::with_capacity(stride, envs.len());
        for e in envs {
            b.push_owned(e);
        }
        b
    }

    pub fn stride(&self) -> usize {
        self.stride
    }

    pub fn len(&self) -> usize {
        self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    pub fn row(&self, i: usize) -> &[Option<Value>] {
        &self.cells[i * self.stride..(i + 1) * self.stride]
    }

    /// The first `rows` rows only: what an instance pushed before it
    /// failed is withdrawn.
    pub fn truncate(&mut self, rows: usize) {
        if rows < self.rows {
            self.rows = rows;
            self.cells.truncate(rows * self.stride);
        }
    }
}

impl Sink for Block {
    fn push_env(&mut self, env: &[Option<Value>]) {
        debug_assert_eq!(env.len(), self.stride);
        self.cells.extend_from_slice(env);
        self.rows += 1;
    }
    fn push_with(&mut self, env: &[Option<Value>], slot: usize, v: Option<Value>) {
        let at = self.cells.len();
        self.cells.extend_from_slice(env);
        self.cells[at + slot] = v;
        self.rows += 1;
    }
    fn push_owned(&mut self, env: Env) {
        debug_assert_eq!(env.len(), self.stride);
        self.cells.extend(env);
        self.rows += 1;
    }
    fn last_row(&mut self) -> Option<&mut [Option<Value>]> {
        if self.rows == 0 {
            return None;
        }
        let at = (self.rows - 1) * self.stride;
        Some(&mut self.cells[at..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(x: f64) -> Option<Value> {
        Some(Value::Num(x))
    }

    #[test]
    fn a_block_holds_rows_in_the_order_pushed_and_withdraws_from_the_end() {
        let mut b = Block::with_capacity(2, 4);
        b.push_env(&[n(1.0), None]);
        b.push_with(&[n(2.0), None], 1, n(3.0));
        b.push_owned(vec![n(4.0), n(5.0)]);
        b.last_row().unwrap()[0] = n(6.0);
        assert_eq!(b.len(), 3);
        assert_eq!(b.row(0), &[n(1.0), None]);
        assert_eq!(b.row(1), &[n(2.0), n(3.0)]);
        assert_eq!(b.row(2), &[n(6.0), n(5.0)]);
        b.truncate(1);
        assert_eq!((b.len(), b.row(0)), (1, &[n(1.0), None][..]));
    }

    #[test]
    fn a_rule_without_variables_has_rows_of_no_slots() {
        let mut b = Block::with_capacity(0, 2);
        b.push_env(&[]);
        b.push_env(&[]);
        assert_eq!(b.len(), 2);
        assert!(b.row(1).is_empty());
        b.truncate(0);
        assert_eq!(b.len(), 0);
        assert!(b.last_row().is_none());
    }
}
