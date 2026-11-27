//! A relation's cross-section at one bar as columns (the block evaluator,
//! `--evaluator block`; ERRATA E-150). A derived relation with one entity
//! argument holds, at a bar, a tuple per entity or a few: its entities form
//! one column and each other argument but the key another. A column whose
//! rows are of one kind holds them unboxed; one whose kinds differ (a split
//! factor that is the Count `1` where no split is and a number where one
//! is) holds the values. An operator over a whole bar (`colplan.rs`) reads
//! its inputs so, and the row path reads the same block as tuples
//! (`ColBlock::rows`), built once. The block memo keeps a relation's
//! blocks by bar beside the row memo, in `Memo`, so that what evicts,
//! clears or shares the one does the other.

use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};

use super::rows::RowSlice;
use super::{Derived, Tuple, Value};

/// One argument of a cross-section: every row's value of one kind, or the
/// values of rows of several kinds.
#[derive(Clone, Debug)]
pub(crate) enum Col {
    Num(Vec<f64>),
    Count(Vec<i64>),
    Time(Vec<i64>),
    Label(Vec<u32>),
    Any(Vec<Value>),
}

impl Col {
    /// An empty column for values like `v`, with room for `rows`.
    pub(crate) fn like(v: &Value, rows: usize) -> Col {
        match v {
            Value::Num(_) => Col::Num(Vec::with_capacity(rows)),
            Value::Count(_) => Col::Count(Vec::with_capacity(rows)),
            Value::Time(_) => Col::Time(Vec::with_capacity(rows)),
            Value::Label(_) => Col::Label(Vec::with_capacity(rows)),
            Value::Equity(_) | Value::Dur(_) | Value::Decision(_) => Col::Any(Vec::with_capacity(rows)),
        }
    }

    /// `v` appended; a value of another kind than the column's turns the
    /// column into one of values.
    pub(crate) fn push(&mut self, v: &Value) {
        match (&mut *self, v) {
            (Col::Num(c), Value::Num(x)) => c.push(*x),
            (Col::Count(c), Value::Count(x)) => c.push(*x),
            (Col::Time(c), Value::Time(x)) => c.push(*x),
            (Col::Label(c), Value::Label(x)) => c.push(*x),
            (Col::Any(c), v) => c.push(v.clone()),
            (typed, v) => {
                let mut vals: Vec<Value> = (0..typed.len()).map(|i| typed.value(i)).collect();
                vals.push(v.clone());
                *typed = Col::Any(vals);
            }
        }
    }

    /// The values as a column: unboxed when they are of one kind.
    pub(crate) fn of_values(vals: Vec<Value>) -> Col {
        let Some(first) = vals.first() else { return Col::Num(Vec::new()) };
        let mut c = Col::like(first, vals.len());
        if matches!(c, Col::Any(_)) || vals.iter().any(|v| std::mem::discriminant(v) != std::mem::discriminant(first)) {
            return Col::Any(vals);
        }
        for v in &vals {
            c.push(v);
        }
        c
    }

    pub(crate) fn len(&self) -> usize {
        match self {
            Col::Num(c) => c.len(),
            Col::Count(c) | Col::Time(c) => c.len(),
            Col::Label(c) => c.len(),
            Col::Any(c) => c.len(),
        }
    }

    /// Row `i`'s value.
    pub(crate) fn value(&self, i: usize) -> Value {
        match self {
            Col::Num(c) => Value::Num(c[i]),
            Col::Count(c) => Value::Count(c[i]),
            Col::Time(c) => Value::Time(c[i]),
            Col::Label(c) => Value::Label(c[i]),
            Col::Any(c) => c[i].clone(),
        }
    }

    /// The rows `idx` names, in its order.
    pub(crate) fn gather(&self, idx: &[u32]) -> Col {
        fn pick<T: Clone>(c: &[T], idx: &[u32]) -> Vec<T> {
            idx.iter().map(|&i| c[i as usize].clone()).collect()
        }
        match self {
            Col::Num(c) => Col::Num(pick(c, idx)),
            Col::Count(c) => Col::Count(pick(c, idx)),
            Col::Time(c) => Col::Time(pick(c, idx)),
            Col::Label(c) => Col::Label(pick(c, idx)),
            Col::Any(c) => Col::Any(pick(c, idx)),
        }
    }

    /// `n` rows of the value `v`.
    pub(crate) fn repeat(v: &Value, n: usize) -> Col {
        match v {
            Value::Num(x) => Col::Num(vec![*x; n]),
            Value::Count(x) => Col::Count(vec![*x; n]),
            Value::Time(x) => Col::Time(vec![*x; n]),
            Value::Label(x) => Col::Label(vec![*x; n]),
            v => Col::Any(vec![v.clone(); n]),
        }
    }
}

/// The rule that derived each row: one for the whole block, or per row.
/// `u32::MAX` stands for a stored relation's rows (`usize::MAX` in a
/// `Derived`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RuleTag {
    One(u32),
    Per(Vec<u32>),
}

impl RuleTag {
    fn at(&self, i: usize) -> usize {
        let r = match self {
            RuleTag::One(r) => *r,
            RuleTag::Per(v) => v[i],
        };
        if r == u32::MAX {
            usize::MAX
        } else {
            r as usize
        }
    }

    fn of(ri: usize) -> u32 {
        u32::try_from(ri).unwrap_or(u32::MAX)
    }
}

/// A relation's tuples at bar `t` as columns, in entity order: `ids` the
/// entities (non-decreasing), `cols` every other argument but the key in
/// signature order. `unique` says no entity has two tuples. The tuples,
/// when a reader asks for them, are built once (`rows`), in this order.
#[derive(Debug)]
pub(crate) struct ColBlock {
    pub t: i64,
    pub key_pos: usize,
    pub entity_pos: usize,
    pub ids: Vec<u32>,
    pub cols: Vec<Col>,
    pub unique: bool,
    pub tags: RuleTag,
    rows: OnceLock<Derived>,
}

impl ColBlock {
    /// A block of columns already built, its tuples not yet.
    pub(crate) fn new(t: i64, key_pos: usize, entity_pos: usize, ids: Vec<u32>, cols: Vec<Col>, tags: RuleTag) -> ColBlock {
        let unique = ids.windows(2).all(|w| w[0] < w[1]);
        debug_assert!(ids.windows(2).all(|w| w[0] <= w[1]), "a block's entities ascend");
        debug_assert!(cols.iter().all(|c| c.len() == ids.len()), "a block's columns have a value per row");
        ColBlock {
            t,
            key_pos,
            entity_pos,
            ids,
            cols,
            unique,
            tags,
            rows: OnceLock::new(),
        }
    }

    /// The columns of `rows`, the tuples of a relation of `arity` arguments
    /// at `t` with their rules, through `values` (a tuple's values by
    /// position). `None` unless every tuple holds `t` at the key and an
    /// instrument at `entity_pos`, and the instruments do not decrease.
    fn build<'a>(t: i64, key_pos: usize, entity_pos: usize, arity: usize, n: usize, row: impl Fn(usize) -> (&'a [Value], usize)) -> Option<ColBlock> {
        if key_pos == entity_pos || key_pos >= arity || entity_pos >= arity {
            return None;
        }
        let mut ids = Vec::with_capacity(n);
        let mut cols: Vec<Option<Col>> = vec![None; arity - 2];
        let mut tags: Vec<u32> = Vec::with_capacity(n);
        for i in 0..n {
            let (tu, ri) = row(i);
            if tu.len() != arity || tu[key_pos] != Value::Time(t) {
                return None;
            }
            let Value::Equity(s) = tu[entity_pos] else { return None };
            if ids.last().is_some_and(|&p| p > s) {
                return None;
            }
            ids.push(s);
            tags.push(RuleTag::of(ri));
            let others = tu.iter().enumerate().filter(|(p, _)| *p != key_pos && *p != entity_pos).map(|(_, v)| v);
            for (slot, v) in cols.iter_mut().zip(others) {
                slot.get_or_insert_with(|| Col::like(v, n)).push(v);
            }
        }
        // A relation with no tuple at the bar: its columns have no kind to
        // keep, and no reader reads a value of them.
        let cols: Vec<Col> = cols.into_iter().map(|c| c.unwrap_or(Col::Num(Vec::new()))).collect();
        let tags = match tags.first() {
            Some(&r) if tags.iter().any(|&x| x != r) => RuleTag::Per(tags),
            Some(&r) => RuleTag::One(r),
            None => RuleTag::One(u32::MAX),
        };
        Some(ColBlock::new(t, key_pos, entity_pos, ids, cols, tags))
    }

    /// The columns of a derived relation's result at `t` (`Kernel::call`'s
    /// order, entity order above `SORTED_MIN` tuples), its tuples kept as
    /// the block's own: `None` where `build` refuses.
    pub(crate) fn from_rows(t: i64, key_pos: usize, entity_pos: usize, arity: usize, d: &Derived) -> Option<ColBlock> {
        let b = ColBlock::build(t, key_pos, entity_pos, arity, d.len(), |i| (d[i].0.as_slice(), d[i].1))?;
        let _ = b.rows.set(d.clone());
        Some(b)
    }

    /// The columns of a stored relation's block at `t` (sorted by entity
    /// by its lookup).
    pub(crate) fn from_slice(t: i64, key_pos: usize, entity_pos: usize, arity: usize, rows: RowSlice<'_>) -> Option<ColBlock> {
        ColBlock::build(t, key_pos, entity_pos, arity, rows.len(), |i| (rows.row(i), usize::MAX))
    }

    pub(crate) fn len(&self) -> usize {
        self.ids.len()
    }

    /// The block's tuples with their rules, in its order: those it was
    /// built from, or built from its columns on the first call (the flag
    /// says this call built them).
    pub(crate) fn rows(&self) -> (Derived, bool) {
        if let Some(d) = self.rows.get() {
            return (d.clone(), false);
        }
        let arity = self.cols.len() + 2;
        let mut out: Vec<(Tuple, usize)> = Vec::with_capacity(self.len());
        for (i, &id) in self.ids.iter().enumerate() {
            let mut tu: Tuple = smallvec::SmallVec::with_capacity(arity);
            let mut others = self.cols.iter();
            for p in 0..arity {
                if p == self.key_pos {
                    tu.push(Value::Time(self.t));
                } else if p == self.entity_pos {
                    tu.push(Value::Equity(id));
                } else {
                    tu.push(others.next().expect("a column per argument").value(i));
                }
            }
            out.push((tu, self.tags.at(i)));
        }
        // Two readers on one bar (worker kernels) build the same tuples;
        // the first one set is kept.
        let built = self.rows.set(Arc::new(out)).is_ok();
        (self.rows.get().expect("set above").clone(), built)
    }

    /// The row of entity `id`, the first when it has several.
    #[cfg(test)]
    pub(crate) fn find(&self, id: u32) -> Option<usize> {
        let i = self.ids.partition_point(|&x| x < id);
        (self.ids.get(i) == Some(&id)).then_some(i)
    }
}

/// A relation's entry in the block memo at a bar: its columns, or the
/// knowledge that its result there has none (`ColBlock::from_rows` refused).
#[derive(Clone, Debug)]
pub(crate) enum BlockEntry {
    Cols(Arc<ColBlock>),
    NotCols,
}

/// The blocks of the derived relations by relation, each relation's by bar
/// ascending.
#[derive(Clone, Debug, Default)]
pub(crate) struct BlockMemo {
    by_rel: Vec<VecDeque<(i64, BlockEntry)>>,
}

impl BlockMemo {
    pub(crate) fn get(&self, rel: usize, t: i64) -> Option<&BlockEntry> {
        let q = self.by_rel.get(rel)?;
        let i = q.partition_point(|(k, _)| *k < t);
        q.get(i).filter(|(k, _)| *k == t).map(|(_, e)| e)
    }

    pub(crate) fn insert(&mut self, rel: usize, t: i64, e: BlockEntry) {
        if self.by_rel.len() <= rel {
            self.by_rel.resize_with(rel + 1, VecDeque::new);
        }
        let q = &mut self.by_rel[rel];
        let i = q.partition_point(|(k, _)| *k < t);
        match q.get_mut(i) {
            Some((k, slot)) if *k == t => *slot = e,
            _ => q.insert(i, (t, e)),
        }
    }

    /// Every block of a bar before `cutoff` dropped (the row memo's cutoff).
    pub(crate) fn evict_before(&mut self, cutoff: i64) {
        for q in &mut self.by_rel {
            while q.front().is_some_and(|(k, _)| *k < cutoff) {
                q.pop_front();
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.by_rel.iter().map(|q| q.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tu(e: u32, t: i64, x: Value) -> Tuple {
        smallvec::smallvec![Value::Equity(e), Value::Time(t), x]
    }

    fn same(a: &Derived, b: &Derived) -> bool {
        a.len() == b.len()
            && a.iter()
                .zip(b.iter())
                .all(|((x, r), (y, s))| r == s && x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| p == q && std::mem::discriminant(p) == std::mem::discriminant(q)))
    }

    /// A block built from tuples and read back as tuples from its columns
    /// alone (the tuples it was built from forgotten).
    fn round_trip(d: &Derived, arity: usize) -> Option<Derived> {
        let b = ColBlock::from_rows(7, 1, 0, arity, d)?;
        let fresh = ColBlock::new(b.t, b.key_pos, b.entity_pos, b.ids.clone(), b.cols.clone(), b.tags.clone());
        let (rows, built) = fresh.rows();
        assert!(built);
        assert!(!fresh.rows().1, "the tuples are built once");
        Some(rows)
    }

    #[test]
    fn tuples_in_entity_order_read_back_from_their_columns_as_they_were() {
        let d: Derived = Arc::new(vec![
            (tu(1, 7, Value::Num(1.5)), 3),
            (tu(2, 7, Value::Num(-0.0)), 3),
            (tu(2, 7, Value::Num(f64::NAN)), 4),
            (tu(5, 7, Value::Num(2.0)), 3),
        ]);
        let back = round_trip(&d, 3).expect("columns");
        assert!(same(&d, &back));
        let b = ColBlock::from_rows(7, 1, 0, 3, &d).unwrap();
        assert!(!b.unique, "entity 2 has two tuples");
        assert_eq!((b.find(2), b.find(5), b.find(3)), (Some(1), Some(3), None));
        assert_eq!(b.tags, RuleTag::Per(vec![3, 3, 4, 3]));
        assert!(same(&b.rows().0, &d) && !b.rows().1, "the block's own tuples are the ones given");
        let mixed: Derived = Arc::new(vec![(tu(1, 7, Value::Num(1.0)), 0), (tu(2, 7, Value::Count(1)), 0), (tu(3, 7, Value::Num(2.0)), 0)]);
        assert!(matches!(ColBlock::from_rows(7, 1, 0, 3, &mixed).unwrap().cols[0], Col::Any(_)));
        assert!(same(&mixed, &round_trip(&mixed, 3).unwrap()), "a column of several kinds keeps each row's");
        let counts: Derived = Arc::new(vec![(tu(0, 7, Value::Count(4)), 0), (tu(9, 7, Value::Count(-1)), 0)]);
        let b = ColBlock::from_rows(7, 1, 0, 3, &counts).unwrap();
        assert!(b.unique && b.tags == RuleTag::One(0));
        assert!(same(&counts, &round_trip(&counts, 3).unwrap()));
        let labels: Derived = Arc::new(vec![(smallvec::smallvec![Value::Label(3), Value::Equity(1), Value::Time(7), Value::Time(5)], 2)]);
        let b = ColBlock::from_rows(7, 2, 1, 4, &labels).unwrap();
        let fresh = ColBlock::new(b.t, b.key_pos, b.entity_pos, b.ids.clone(), b.cols.clone(), b.tags.clone());
        assert!(same(&labels, &fresh.rows().0));
        let bare: Derived = Arc::new(vec![(smallvec::smallvec![Value::Equity(4), Value::Time(7)], 1)]);
        assert!(same(&bare, &round_trip(&bare, 2).unwrap()));
        let empty: Derived = Arc::new(Vec::new());
        assert!(same(&empty, &round_trip(&empty, 3).unwrap()));
    }

    #[test]
    fn unsorted_entities_and_other_bars_have_no_columns() {
        let unsorted: Derived = Arc::new(vec![(tu(2, 7, Value::Num(1.0)), 0), (tu(1, 7, Value::Num(1.0)), 0)]);
        assert!(ColBlock::from_rows(7, 1, 0, 3, &unsorted).is_none());
        let other_bar: Derived = Arc::new(vec![(tu(1, 6, Value::Num(1.0)), 0)]);
        assert!(ColBlock::from_rows(7, 1, 0, 3, &other_bar).is_none());
        let no_entity: Derived = Arc::new(vec![(smallvec::smallvec![Value::Num(1.0), Value::Time(7), Value::Num(1.0)], 0)]);
        assert!(ColBlock::from_rows(7, 1, 0, 3, &no_entity).is_none());
        let duration: Derived = Arc::new(vec![(tu(1, 7, Value::Dur(Box::default())), 0)]);
        assert!(same(&duration, &round_trip(&duration, 3).unwrap()), "a duration is held as a value");
    }

    #[test]
    fn the_block_memo_holds_each_relations_bars_in_order_and_drops_the_old() {
        let mut m = BlockMemo::default();
        for t in [5, 3, 9, 7] {
            m.insert(2, t, BlockEntry::NotCols);
        }
        m.insert(0, 4, BlockEntry::NotCols);
        assert!(m.get(2, 7).is_some() && m.get(2, 6).is_none() && m.get(1, 7).is_none() && m.get(9, 7).is_none());
        assert_eq!(m.len(), 5);
        m.evict_before(6);
        assert_eq!(m.len(), 2);
        assert!(m.get(2, 5).is_none() && m.get(2, 9).is_some() && m.get(0, 4).is_none());
    }
}
