//! S2 Classify (ST-7, ST-8.1): the normalised rows against the current
//! version, per instrument: appends (after its last row), duplicates
//! (identical to a stored row), revisions (a stored row with another
//! value or receipt), late rows (inside its history, absent); and the
//! instrument's final rows and revision entries, which S3 builds from.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

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
    /// Its final rows in label order: one per label, the latest by receipt;
    /// none for an instrument kept as the previous version holds it (E-122).
    pub rows: Vec<Row>,
    /// The rows superseded at a label, each with the receipt that
    /// superseded it.
    pub revisions: Vec<Revision>,
    /// Rebuilt by S3 (new or changed), else its runs are kept.
    pub rebuild: bool,
}

/// A stored instrument's rows and revisions, as rows: one the version's
/// runs hold, or one only its overlay holds (appended since publication as
/// a new instrument, E-121).
static ROWS_READ: AtomicU64 = AtomicU64::new(0);

/// The stored rows staging has read back so far in this process (E-122's
/// test).
pub fn rows_read() -> u64 {
    ROWS_READ.load(Ordering::Relaxed)
}

pub(crate) fn stored_rows(prev: &HotVersion, id: u64) -> (Vec<Row>, Vec<Row>) {
    // The current row of each label (the superseded ones come from the
    // revision table, not from the read, or they would count twice).
    let f = crate::store::RowFilter {
        identity: Some(vec![id]),
        latest_only: true,
        ..crate::store::RowFilter::default()
    };
    let rows = prev.read(&f, 0, prev.presence().labels.len() as u32).expect("a stored instrument reads");
    ROWS_READ.fetch_add(rows.len() as u64, Ordering::Relaxed);
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
        .filter(|x| x.id == id)
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
    // Every stored instrument, kept unless the input touches it or it was
    // appended to since publication (its rows are not all in its runs,
    // ST-8): then it is rebuilt from its rows as read.
    let appended = |p: &HotVersion, id: u64| -> bool {
        let ov = p.overlay.lock().unwrap_or_else(|e| e.into_inner());
        ov.by_id.get(&id).is_some_and(|&o| ov.instruments[o].rows() > 0)
    };
    if let Some(p) = prev {
        for inst in p.presence().instruments.iter() {
            if !input.by_id.contains_key(&inst.id) {
                let revisions = p.manifest.revisions.iter().filter(|x| x.id == inst.id).cloned().collect();
                let rebuild = appended(p, inst.id);
                // A kept instrument's rows are not read (E-122): its
                // presence, runs and ladder come from the version, so an
                // ingest's memory follows what it writes, not the store.
                let rows = if rebuild { stored_rows(p, inst.id).0 } else { Vec::new() };
                planned.insert(
                    inst.id,
                    Planned {
                        id: inst.id,
                        rows,
                        revisions,
                        rebuild,
                    },
                );
                if rebuild {
                    counts.instruments_rebuilt += 1;
                } else {
                    counts.instruments_kept += 1;
                }
            }
        }
        // The instruments appended since publication as new ones: in the
        // overlay alone, so the loop above, over the published ones, never
        // saw them, and the version dropped their rows, every row of every
        // futures root ingested after the first when a later ingest was no
        // append (E-121). Rebuilt from their rows as read.
        let overlay_only: Vec<u64> = {
            let ov = p.overlay.lock().unwrap_or_else(|e| e.into_inner());
            let mut ids: Vec<u64> = ov.by_id.keys().copied().filter(|id| p.presence().ordinal(*id).is_none() && !input.by_id.contains_key(id)).collect();
            ids.sort_unstable();
            ids
        };
        for id in overlay_only {
            let (rows, _) = stored_rows(p, id);
            if rows.is_empty() {
                continue;
            }
            planned.insert(
                id,
                Planned {
                    id,
                    rows,
                    revisions: Vec::new(),
                    rebuild: true,
                },
            );
            counts.instruments_rebuilt += 1;
        }
    }
    for (&id, rows) in &input.by_id {
        let (stored, superseded) = match prev {
            Some(p) if p.presence().ordinal(id).is_some() || appended(p, id) => stored_rows(p, id),
            _ => (Vec::new(), Vec::new()),
        };
        let fresh = stored.is_empty();
        let last_label = stored.last().map(|x| x.t);
        let was_appended = prev.is_some_and(|p| appended(p, id));
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
        let changed = changed || was_appended;
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
