//! S2 Classify (ST-7, ST-8.1): the normalised rows against the current
//! version, per instrument: appends (after its last row), duplicates
//! (identical to a stored row), revisions (a stored row with another
//! value or receipt), late rows (inside its history, absent); and the
//! instrument's final rows and revision entries, which S3 builds from.

use std::collections::BTreeMap;

use super::normalise::{Normalised, Row};
use crate::hot::manifest::{Kind, Revision};
use crate::hot::HotVersion;

/// What an ingest did to the store, counted (the stage's report).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Counts {
    pub instruments_new: usize,
    pub instruments_rebuilt: usize,
    pub instruments_kept: usize,
    pub appends: u64,
    pub duplicates: u64,
    pub revisions: u64,
    pub late: u64,
}

/// An instrument of the next version.
#[derive(Clone, Debug)]
pub struct Planned {
    pub id: u64,
    /// Its final rows in label order: one per label, the latest by receipt.
    pub rows: Vec<Row>,
    /// The rows superseded at a label, each with the receipt that
    /// superseded it.
    pub revisions: Vec<Revision>,
    /// Rebuilt by S3 (new or changed), else its runs are kept.
    pub rebuild: bool,
}

/// A stored instrument's rows and revisions, as rows.
fn stored_rows(prev: &HotVersion, ordinal: usize) -> (Vec<Row>, Vec<Row>) {
    let inst = &prev.presence.instruments[ordinal];
    let f = crate::store::RowFilter {
        identity: Some(vec![inst.id]),
        ..crate::store::RowFilter::default()
    };
    let rows = prev.read(&f, 0, prev.presence.labels.len() as u32).expect("a stored instrument reads");
    let kinds: Vec<Kind> = prev.manifest.columns.iter().map(|c| c.kind).collect();
    let mut current = Vec::with_capacity(rows.len());
    for i in 0..rows.len() {
        let vals: Vec<u64> = (0..prev.value_columns())
            .map(|c| match kinds[c] {
                Kind::Tick | Kind::TickF32 => rows.floats[c][i].to_bits(),
                Kind::Int | Kind::Residual => rows.ints[c][i] as u64,
            })
            .collect();
        current.push(Row {
            t: rows.t[i],
            r: rows.r[i],
            seq: rows.seq[i],
            vals,
        });
    }
    let superseded = prev
        .manifest
        .revisions
        .iter()
        .filter(|x| x.id == inst.id)
        .map(|x| Row {
            t: x.label,
            r: x.r,
            seq: x.seq,
            vals: x.values.clone(),
        })
        .collect();
    (current, superseded)
}

/// Plans the next version's instruments from the input and the current
/// version; new rows are numbered from `next_seq` in (instrument, time,
/// receipt) order, the log's order.
pub fn classify(prev: Option<&HotVersion>, input: &Normalised, next_seq: u64) -> (Vec<Planned>, Counts, u64) {
    let mut counts = Counts::default();
    let mut seq = next_seq;
    let mut planned: BTreeMap<u64, Planned> = BTreeMap::new();
    // Every stored instrument, kept unless the input touches it.
    if let Some(p) = prev {
        for (o, inst) in p.presence.instruments.iter().enumerate() {
            if !input.by_id.contains_key(&inst.id) {
                let (rows, _) = stored_rows(p, o);
                let revisions = p.manifest.revisions.iter().filter(|x| x.id == inst.id).cloned().collect();
                planned.insert(
                    inst.id,
                    Planned {
                        id: inst.id,
                        rows,
                        revisions,
                        rebuild: false,
                    },
                );
                counts.instruments_kept += 1;
            }
        }
    }
    for (&id, rows) in &input.by_id {
        let (stored, superseded) = match prev.and_then(|p| p.presence.ordinal(id).map(|o| (p, o))) {
            Some((p, o)) => stored_rows(p, o as usize),
            None => (Vec::new(), Vec::new()),
        };
        let fresh = stored.is_empty();
        let last_label = stored.last().map(|x| x.t);
        // Every row known of each label: stored current, stored superseded,
        // then the input's, numbered as they are accepted.
        let mut by_label: BTreeMap<i64, Vec<Row>> = BTreeMap::new();
        for x in stored.iter().chain(&superseded) {
            by_label.entry(x.t).or_default().push(x.clone());
        }
        let mut changed = fresh;
        for x in rows {
            let known = by_label.entry(x.t).or_default();
            let numbered = Row { seq, ..x.clone() };
            seq += 1;
            if known.iter().any(|k| k.r == x.r && k.vals == x.vals) {
                counts.duplicates += 1;
                continue;
            }
            if fresh || last_label.is_some_and(|l| x.t > l) {
                counts.appends += 1;
            } else if known.is_empty() {
                counts.late += 1;
            } else {
                counts.revisions += 1;
            }
            known.push(numbered);
            changed = true;
        }
        let mut final_rows = Vec::with_capacity(by_label.len());
        let mut revisions = Vec::new();
        for (t, mut known) in by_label {
            known.sort_by_key(|k| (k.r, k.seq));
            let current = known.pop().expect("a label with a row");
            for k in known {
                revisions.push(Revision {
                    id,
                    label: t,
                    r: k.r,
                    r_to: current.r,
                    seq: k.seq,
                    values: k.vals,
                });
            }
            final_rows.push(current);
        }
        revisions.sort_by_key(|x| (x.label, x.r, x.seq));
        if fresh {
            counts.instruments_new += 1;
        } else if changed {
            counts.instruments_rebuilt += 1;
        } else {
            counts.instruments_kept += 1;
        }
        planned.insert(
            id,
            Planned {
                id,
                rows: final_rows,
                revisions,
                rebuild: changed,
            },
        );
    }
    (planned.into_values().collect(), counts, seq)
}
