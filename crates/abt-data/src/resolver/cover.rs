//! The cheapest cover of a span (Proposition D-7.4): a weighted interval
//! cover, by dynamic programming over the candidates in the order of their
//! ends (Kleinberg and Tardos, *Algorithm Design*, ch. 6). A cost is any
//! monoid ordered compatibly with its addition (the lexicographic order of
//! D-7.3 on componentwise sums is one), so the cheapest cover ending with a
//! candidate extends the cheapest cover of what precedes it.
//!
//! The candidates of one need are a handful of sources, so the recurrence
//! is evaluated directly, in O(m²); D-7.4's O(m log m) needs a
//! range-minimum structure over the ends that this scale does not.

use std::cmp::Ordering;
use std::ops::Add;

/// A candidate: the instants `[from, to]` it covers, both included, and
/// its cost.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Interval<C> {
    pub from: i64,
    pub to: i64,
    pub cost: C,
}

/// The cheapest chain of candidates covering `[from, to]`, each starting no
/// later than `gap` after the end of the one before it (the bars between
/// two sources' spans: a weekend, a holiday), with its total. Among covers
/// of equal cost, the one whose candidates come first in `cands` (compared
/// in order) wins, so the order of the candidates is the tie-break. `None`
/// when no chain covers the span.
pub fn cover<C: Copy + Add<Output = C>>(from: i64, to: i64, gap: i64, cands: &[Interval<C>], cmp: impl Fn(&C, &C) -> Ordering) -> Option<(C, Vec<usize>)> {
    // A candidate can take part when it reaches `from` and starts within
    // `gap` of `to` (the last may bridge the end the way any two abut).
    let mut by_end: Vec<usize> = (0..cands.len())
        .filter(|&i| cands[i].to >= from && cands[i].from <= to.saturating_add(gap) && cands[i].from <= cands[i].to)
        .collect();
    by_end.sort_by_key(|&i| (cands[i].to, i));
    // The cheapest chain covering [from, cands[i].to] that ends with i.
    let mut best: Vec<Option<(C, Vec<usize>)>> = vec![None; cands.len()];
    let better = |a: &(C, Vec<usize>), b: &(C, Vec<usize>)| match cmp(&a.0, &b.0) {
        Ordering::Equal => a.1 < b.1,
        o => o == Ordering::Less,
    };
    for (pos, &i) in by_end.iter().enumerate() {
        let c = &cands[i];
        let mut found: Option<(C, Vec<usize>)> = (c.from <= from).then(|| (c.cost, vec![i]));
        for &j in &by_end[..pos] {
            let Some((cost, chain)) = &best[j] else { continue };
            // j must end inside reach of i's start, and i must reach further.
            if c.from <= cands[j].to.saturating_add(gap) && c.to > cands[j].to {
                let mut chain = chain.clone();
                chain.push(i);
                let candidate = (*cost + c.cost, chain);
                if found.as_ref().is_none_or(|f| better(&candidate, f)) {
                    found = Some(candidate);
                }
            }
        }
        best[i] = found;
    }
    let mut answer: Option<(C, Vec<usize>)> = None;
    for &i in &by_end {
        if cands[i].to < to {
            continue;
        }
        if let Some(b) = &best[i] {
            if answer.as_ref().is_none_or(|a| better(b, a)) {
                answer = Some(b.clone());
            }
        }
    }
    answer
}
