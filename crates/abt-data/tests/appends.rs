//! Appends to a hot relation (docs/spec/07-storage.md section 8): rows
//! after an instrument's stored history join its tail under the published
//! version, visible by sequence number (Proposition 8.1); tails of 1,024
//! rows are sealed into append files and journaled; a restart rebuilds the
//! tails from the log and the sealed blocks from the journal (Proposition
//! 8.2), a torn journal or append file truncated; consolidation folds the
//! journaled blocks into base segments under the same version id, every
//! answer unchanged (ST-8.7); the `map` request describes the segments,
//! append files and tails (ST-10.2) and a reader decoding a mapped segment
//! with `abt-codec` reads what a query answers.
//!
//! Every read form is compared with a Parquet-store twin ingesting the
//! same batches (the twin makes a version per batch; the hot store keeps
//! its one version).

mod hot_common;

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use abt_data::hot::append::Overlay;
use abt_data::hot::segment::Segment;
use abt_data::query::ResolvedView;
use abt_data::{RowFilter, Store};
use abt_wire::scalar::{NANOS_PER_DAY, NANOS_PER_HOUR};
use arrow::array::{AsArray, RecordBatch};
use hot_common::*;

const REL: &str = "equity.close";

/// A market row: instrument, label, receipt, price.
type Row = (u64, i64, i64, f64);

/// Snapshots recorded by the appender: a sequence and the reads at it.
type Snapshots = Arc<std::sync::Mutex<Vec<(u64, Vec<String>)>>>;

/// The market's rows split into the published history (before day
/// `split`) and the day batches after it, `width` days a batch.
fn batches(rows: &[Row], start: i64, split: i64, days: i64, width: i64) -> (Vec<Row>, Vec<Vec<Row>>) {
    let t_of = |d: i64| (start + d) * NANOS_PER_DAY;
    let base: Vec<_> = rows.iter().filter(|r| r.1 < t_of(split)).cloned().collect();
    let mut out = Vec::new();
    let mut d = split;
    while d < days {
        let b: Vec<_> = rows.iter().filter(|r| r.1 >= t_of(d) && r.1 < t_of(d + width)).cloned().collect();
        if !b.is_empty() {
            out.push(b);
        }
        d += width;
    }
    (base, out)
}

/// The text of a read, a snapshot at `seq` when given.
fn read_text(s: &Store, v: &str, f: &RowFilter, seq: Option<u64>) -> String {
    text_with_seq(&s.read(v, &RowFilter { as_of_seq: seq, ..f.clone() }).unwrap())
}

fn seq_high(s: &Store, v: &str) -> u64 {
    s.hot(v).unwrap().seq_high()
}

#[test]
fn appends_read_as_the_parquet_twin_and_keep_the_version() {
    let mut rng = Rng(11);
    let ids = [3u64, 5, 8, 13, 21];
    let (start, days) = (18_000i64, 4_000i64);
    let rows = market(&mut rng, &ids, start, days);
    let (base, day_batches) = batches(&rows, start, 1_100, days, 10);
    assert!(ids.iter().all(|id| base.iter().any(|r| r.0 == *id)), "every instrument has published rows");
    let mut a = Store::temporary("app-a").unwrap();
    let mut b = Store::temporary("app-b").unwrap();
    let va = a.ingest(REL, &[close_rows(&base)], "test", hot()).unwrap();
    let mut vb = b.ingest(REL, &[close_rows(&base)], "test", BTreeMap::new()).unwrap();
    assert!(a.hot(&va).unwrap().overlay.lock().unwrap().is_empty());
    for (i, batch) in day_batches.iter().enumerate() {
        // An all-append batch takes the append path: the version stands.
        let v = a.ingest(REL, &[close_rows(batch)], "test", hot()).unwrap();
        assert_eq!(v, va, "an append keeps the version (ST-8.1)");
        vb = b.ingest(REL, &[close_rows(batch)], "test", BTreeMap::new()).unwrap();
        if i % 40 == 39 {
            same_reads(&a, &va, &b, &vb, &mut rng, &ids, start, days);
        }
    }
    // Seals happened: the batches took some instrument past 1,024 rows.
    let h = a.hot(&va).unwrap();
    {
        let ov = h.overlay.lock().unwrap();
        let sealed: usize = ov.instruments.iter().map(|i| i.sealed.len()).sum();
        assert!(sealed >= 2, "{} blocks sealed", sealed);
        assert_eq!(ov.journal_records, sealed as u64);
        assert!(ov.instruments.iter().any(|i| !i.tail.is_empty()), "a tail remains");
    }
    same_reads(&a, &va, &b, &vb, &mut rng, &ids, start, days);
    // The registry's record of the version counts the appended rows.
    assert_eq!(a.registry.version(&va).unwrap().rows, b.registry.version(&vb).unwrap().rows);

    // A late row at a new label inside a tail's span inserts under a new
    // sequence number (ST-8.5): the version stands; a snapshot before it
    // does not see it. A row at a label the tail holds is a revision: a
    // version (E-90).
    let (tail_id, tail_label, tail_r) = {
        let ov = h.overlay.lock().unwrap();
        let inst = ov.instruments.iter().find(|i| i.tail.len() >= 3).expect("a tail of three rows");
        (inst.id, inst.tail.label_at(1), inst.tail.r_at(1))
    };
    let inside = (tail_id, tail_label + NANOS_PER_HOUR, tail_r + NANOS_PER_DAY, 123.45);
    let before = seq_high(&a, &va);
    let v = a.ingest(REL, &[close_rows(&[inside])], "test", hot()).unwrap();
    assert_eq!(v, va, "an insert at a new label keeps the version");
    vb = b.ingest(REL, &[close_rows(&[inside])], "test", BTreeMap::new()).unwrap();
    let f = RowFilter {
        identity: Some(vec![inside.0]),
        t0: Some(inside.1 - 10 * NANOS_PER_DAY),
        ..RowFilter::default()
    };
    assert_same("in-tail insert", &text(&a.read(&va, &f).unwrap()), &text(&b.read(&vb, &f).unwrap()));
    let snap = read_text(&a, &va, &f, Some(before));
    assert!(!snap.contains("123.45"), "a snapshot before the insert does not see it:\n{}", snap);
    assert!(read_text(&a, &va, &f, None).contains("123.45"));
    let correction = (tail_id, tail_label, tail_r + 2 * NANOS_PER_DAY, 321.5);
    let vx = a.ingest(REL, &[close_rows(&[correction])], "test", hot()).unwrap();
    assert_ne!(vx, va, "a row at a label the tail holds makes a version");
    let vbx = b.ingest(REL, &[close_rows(&[correction])], "test", BTreeMap::new()).unwrap();
    assert_same("tail revision", &text(&a.read(&vx, &f).unwrap()), &text(&b.read(&vbx, &f).unwrap()));
    assert_same(
        "tail revision, latest",
        &text(&a.read(&vx, &RowFilter { latest_only: true, ..f.clone() }).unwrap()),
        &text(&b.read(&vbx, &RowFilter { latest_only: true, ..f.clone() }).unwrap()),
    );
    let _ = vbx;
    assert!(!read_text(&a, &va, &f, None).contains("321.5"), "the earlier version does not hold the revision");

    // A revision of a sealed row makes a version (S2 to S5) that folds the
    // overlay; both versions read as their twins.
    let rev_of = rows.iter().find(|r| r.0 == ids[0] && r.1 >= (start + 1_100) * NANOS_PER_DAY).unwrap();
    let revised = (rev_of.0, rev_of.1, rev_of.2 + 7 * NANOS_PER_DAY, rev_of.3 + 1.0);
    let va2 = a.ingest(REL, &[close_rows(&[revised])], "test", hot()).unwrap();
    assert_ne!(va2, vx, "a revision makes a version");
    let vb2 = b.ingest(REL, &[close_rows(&[revised])], "test", BTreeMap::new()).unwrap();
    same_reads(&a, &va2, &b, &vb2, &mut rng, &ids, start, days);
    same_reads(&a, &vx, &b, &vbx, &mut rng, &ids, start, days);
    assert!(a.hot(&va2).unwrap().overlay.lock().unwrap().is_empty(), "the new version starts with no appends");
    // Appends continue under the new version.
    let more = (ids[1], (start + days + 5) * NANOS_PER_DAY, (start + days + 5) * NANOS_PER_DAY + NANOS_PER_HOUR, 50.5);
    assert_eq!(a.ingest(REL, &[close_rows(&[more])], "test", hot()).unwrap(), va2);
    let vb3 = b.ingest(REL, &[close_rows(&[more])], "test", BTreeMap::new()).unwrap();
    same_reads(&a, &va2, &b, &vb3, &mut rng, &ids, start, days + 6);
}

#[test]
fn readers_at_a_sequence_keep_their_view_under_appends() {
    // Proposition 8.1 under interleaving: readers snapshot at sequences
    // while another thread appends (and seals); every snapshot equals the
    // one taken when its sequence was the high-water.
    let mut rng = Rng(12);
    let ids = [1u64, 2, 3];
    let (start, days) = (18_000i64, 2_400i64);
    let rows = market(&mut rng, &ids, start, days);
    let (base, day_batches) = batches(&rows, start, 700, days, 5);
    let store = Arc::new(RwLock::new(Store::temporary("app-threads").unwrap()));
    let va = store.write().unwrap().ingest(REL, &[close_rows(&base)], "test", hot()).unwrap();
    let filters: Vec<RowFilter> = vec![
        RowFilter::default(),
        RowFilter {
            identity: Some(vec![2]),
            ..RowFilter::default()
        },
        RowFilter {
            t0: Some((start + 690) * NANOS_PER_DAY),
            t1: Some((start + 1_500) * NANOS_PER_DAY),
            latest_only: true,
            ..RowFilter::default()
        },
    ];
    let expected: Snapshots = Arc::new(std::sync::Mutex::new(Vec::new()));
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let checked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let readers: Vec<_> = (0..3)
        .map(|_| {
            let (store, expected, done, checked, va, filters) = (store.clone(), expected.clone(), done.clone(), checked.clone(), va.clone(), filters.clone());
            std::thread::spawn(move || {
                let mut checks = 0usize;
                while !done.load(std::sync::atomic::Ordering::SeqCst) {
                    let snapshots: Vec<(u64, Vec<String>)> = expected.lock().unwrap().clone();
                    let st = store.read().unwrap();
                    for (seq, texts) in snapshots.iter().rev().take(4) {
                        for (f, t) in filters.iter().zip(texts) {
                            assert_eq!(&read_text(&st, &va, f, Some(*seq)), t, "a reader at {} sees its rows", seq);
                            checks += 1;
                            checked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                    }
                }
                checks
            })
        })
        .collect();
    for batch in &day_batches {
        {
            let mut st = store.write().unwrap();
            st.ingest(REL, &[close_rows(batch)], "test", hot()).unwrap();
            let seq = seq_high(&st, &va);
            let texts: Vec<String> = filters.iter().map(|f| read_text(&st, &va, f, Some(seq))).collect();
            expected.lock().unwrap().push((seq, texts));
        }
        // The readers check between appends: a writer taking the lock
        // again at once kept them out (a waiting writer blocks new
        // readers), and they could end having checked nothing. A reader
        // ends before `done` only by failing its check: the wait stops
        // then, and the join below reports it at once.
        let seen = checked.load(std::sync::atomic::Ordering::SeqCst);
        let waited = std::time::Instant::now();
        while checked.load(std::sync::atomic::Ordering::SeqCst) == seen && waited.elapsed() < std::time::Duration::from_secs(5) && !readers.iter().any(|r| r.is_finished()) {
            std::thread::yield_now();
        }
    }
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    let checks: usize = readers.into_iter().map(|r| r.join().unwrap()).sum();
    assert!(checks > 0, "the readers checked snapshots");
    // Every recorded snapshot still holds at the end.
    let st = store.read().unwrap();
    for (seq, texts) in expected.lock().unwrap().iter() {
        for (f, t) in filters.iter().zip(texts) {
            assert_eq!(&read_text(&st, &va, f, Some(*seq)), t);
        }
    }
}

/// A persistent store under the scratch directory, for restarts.
fn persistent(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("abt-appends-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn a_restart_answers_what_it_answered_before() {
    let mut rng = Rng(13);
    let ids = [4u64, 6];
    let (start, days) = (18_000i64, 4_000i64);
    let rows = market(&mut rng, &ids, start, days);
    let (base, day_batches) = batches(&rows, start, 1_100, days, 20);
    let dir = persistent("restart");
    let filters: Vec<RowFilter> = vec![
        RowFilter::default(),
        RowFilter {
            identity: Some(vec![6]),
            t0: Some((start + 1_500) * NANOS_PER_DAY),
            ..RowFilter::default()
        },
        RowFilter {
            latest_only: true,
            r_max: Some((start + 3_000) * NANOS_PER_DAY),
            ..RowFilter::default()
        },
    ];
    let (va, answers, high, sealed) = {
        let mut a = Store::open(&dir).unwrap();
        let va = a.ingest(REL, &[close_rows(&base)], "test", hot()).unwrap();
        for batch in &day_batches {
            a.ingest(REL, &[close_rows(batch)], "test", hot()).unwrap();
        }
        let high = seq_high(&a, &va);
        let h = a.hot(&va).unwrap();
        let sealed: usize = h.overlay.lock().unwrap().instruments.iter().map(|i| i.sealed.len()).sum();
        assert!(sealed >= 2, "{} blocks sealed", sealed);
        let seqs = [high, high - 1, high / 2, high - 1_500, high - 3_000];
        let mut answers = Vec::new();
        for f in &filters {
            for s in seqs {
                answers.push(read_text(&a, &va, f, Some(s)));
            }
            answers.push(read_text(&a, &va, f, None));
        }
        let m = a.manifest(&va).unwrap();
        answers.push(format!("{:?}", m.partitions.iter().map(|p| (&p.bucket, p.rows, p.t_min, p.t_max, p.seq_max)).collect::<Vec<_>>()));
        (va, answers, high, sealed)
    };
    let check = |what: &str| {
        let a = Store::open(&dir).unwrap();
        let mut got = Vec::new();
        for f in &filters {
            for s in [high, high - 1, high / 2, high - 1_500, high - 3_000] {
                got.push(read_text(&a, &va, f, Some(s)));
            }
            got.push(read_text(&a, &va, f, None));
        }
        let m = a.manifest(&va).unwrap();
        got.push(format!("{:?}", m.partitions.iter().map(|p| (&p.bucket, p.rows, p.t_min, p.t_max, p.seq_max)).collect::<Vec<_>>()));
        for (i, (x, y)) in got.iter().zip(&answers).enumerate() {
            assert_same(&format!("{}: answer {}", what, i), x, y);
        }
        assert_eq!(seq_high(&a, &va), high, "{}: the high-water", what);
        a
    };
    // A clean restart: the journal replayed, the tails rebuilt.
    let a = check("clean restart");
    let vdir = a.root().join(format!("hot/{}/{}", REL, va));
    let journal = Overlay::journal_file(&vdir, 0);
    let recs = a.hot(&va).unwrap().overlay.lock().unwrap().journal_records;
    assert_eq!(recs as usize, sealed);
    drop(a);
    // A torn journal: its last record cut short. The block it named is no
    // longer visible as sealed; its rows come back from the log.
    let bytes = std::fs::read(&journal).unwrap();
    std::fs::write(&journal, &bytes[..bytes.len() - 7]).unwrap();
    let a = check("torn journal");
    let h = a.hot(&va).unwrap();
    assert_eq!(h.overlay.lock().unwrap().journal_records as usize, sealed - 1, "the torn record is dropped");
    assert_eq!(
        std::fs::metadata(&journal).unwrap().len(),
        bytes.len() as u64 - 7 - (bytes.len() as u64 - 7 - journaled_len(&bytes[..bytes.len() - 7])),
        "the journal is truncated to its last good record"
    );
    drop(h);
    drop(a);
    // Bytes past the last journaled block of an append file (a crash
    // between a block and its record): truncated at open, never visible.
    let append_dir = Overlay::append_dir(&vdir, 0);
    let file = std::fs::read_dir(&append_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.file_name().unwrap().to_string_lossy().starts_with("P-"))
        .unwrap();
    let len = std::fs::metadata(&file).unwrap().len();
    let mut junk = std::fs::read(&file).unwrap();
    junk.extend_from_slice(&[0xAB; 777]);
    std::fs::write(&file, &junk).unwrap();
    let a = check("junk past the journaled end");
    assert!(std::fs::metadata(&file).unwrap().len() <= len, "the append file is truncated to its journaled end");
    // The store goes on: more appends, a restart, the twin agrees.
    drop(a);
    let mut a = Store::open(&dir).unwrap();
    let mut b = Store::temporary("restart-twin").unwrap();
    b.ingest(REL, &[close_rows(&base)], "test", BTreeMap::new()).unwrap();
    for batch in &day_batches {
        b.ingest(REL, &[close_rows(batch)], "test", BTreeMap::new()).unwrap();
    }
    let extra = market(&mut rng, &ids, start + days, 40);
    a.ingest(REL, &[close_rows(&extra)], "test", hot()).unwrap();
    let vb = b.ingest(REL, &[close_rows(&extra)], "test", BTreeMap::new()).unwrap();
    drop(a);
    let a = Store::open(&dir).unwrap();
    same_reads(&a, &va, &b, &vb, &mut rng, &ids, start, days + 40);
    drop(a);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Daily rows of `id` over `days` days from day `d0` of the calendar
/// starting at `start`.
fn daily(id: u64, start: i64, d0: i64, days: i64) -> Vec<Row> {
    (d0..d0 + days)
        .map(|d| (id, (start + d) * NANOS_PER_DAY, (start + d) * NANOS_PER_DAY, 10.0 + id as f64 + ((d * 7) % 13) as f64 * 0.25))
        .collect()
}

/// The blocks sealed, the journal's records and each instrument's tail
/// rows, by id.
fn sealed_and_tails(s: &Store, v: &str) -> (usize, u64, BTreeMap<u64, usize>) {
    let h = s.hot(v).unwrap();
    let ov = h.overlay.lock().unwrap();
    (
        ov.instruments.iter().map(|i| i.sealed.len()).sum(),
        ov.journal_records,
        ov.instruments.iter().map(|i| (i.id, i.tail.len())).collect(),
    )
}

/// ERRATA E-137: a seal record names its instrument by its id. Two
/// instruments arrive after the publication; the one that arrived second
/// seals first. A restart numbers them in the order of their seal records,
/// the write numbered them in the order of their rows: the record is the
/// instrument's all the same, the block stays sealed, and the tails hold
/// under a block each, restart after restart.
#[test]
fn a_seal_record_of_an_instrument_that_arrived_later_survives_a_restart() {
    let mut rng = Rng(137);
    let ids = [3u64, 5, 8];
    let start = 18_000i64;
    let dir = persistent("e137");
    let mut b = Store::temporary("e137-twin").unwrap();
    // The published history: instrument 3 alone.
    let base = daily(3, start, 0, 600);
    // 5 arrives first with 300 rows and seals nothing; 8 arrives with it,
    // after it in the batch, and seals a block.
    let first: Vec<Row> = [daily(5, start, 600, 300), daily(8, start, 600, 1_300)].concat();
    let va = {
        let mut a = Store::open(&dir).unwrap();
        let va = a.ingest(REL, &[close_rows(&base)], "test", hot()).unwrap();
        assert_eq!(a.ingest(REL, &[close_rows(&first)], "test", hot()).unwrap(), va);
        assert_eq!(sealed_and_tails(&a, &va), (1, 1, BTreeMap::from([(3, 0), (5, 300), (8, 276)])));
        va
    };
    b.ingest(REL, &[close_rows(&base)], "test", BTreeMap::new()).unwrap();
    let mut vb = b.ingest(REL, &[close_rows(&first)], "test", BTreeMap::new()).unwrap();
    // The restart: the block is sealed still, its rows in no tail.
    {
        let a = Store::open(&dir).unwrap();
        assert_eq!(sealed_and_tails(&a, &va), (1, 1, BTreeMap::from([(3, 0), (5, 300), (8, 276)])), "after a restart");
        same_reads(&a, &va, &b, &vb, &mut rng, &ids, start, 1_900);
    }
    // The store goes on, a process a span as a staging runs: each
    // instrument seals as its tail fills, and every seal is kept.
    let second: Vec<Row> = [daily(5, start, 1_900, 1_000), daily(8, start, 1_900, 1_000)].concat();
    {
        let mut a = Store::open(&dir).unwrap();
        assert_eq!(a.ingest(REL, &[close_rows(&second)], "test", hot()).unwrap(), va);
    }
    vb = b.ingest(REL, &[close_rows(&second)], "test", BTreeMap::new()).unwrap();
    let a = Store::open(&dir).unwrap();
    assert_eq!(sealed_and_tails(&a, &va), (3, 3, BTreeMap::from([(3, 0), (5, 276), (8, 252)])), "after the second span and a restart");
    same_reads(&a, &va, &b, &vb, &mut rng, &ids, start, 2_900);
    drop(a);
    let _ = std::fs::remove_dir_all(&dir);
}

/// ERRATA E-137, the store it left: a journal cut at a restart leaves
/// tails of several blocks, and an instrument that receives no more rows
/// is never sealed by an append. A consolidation seals every tail that is
/// due before it folds: the tails hold under a block each again, and every
/// read answers as before.
#[test]
fn a_consolidation_seals_the_tails_a_cut_journal_left() {
    let mut rng = Rng(138);
    let ids = [3u64, 5, 8];
    let start = 18_000i64;
    let dir = persistent("e137-heal");
    let mut b = Store::temporary("e137-heal-twin").unwrap();
    let base = daily(3, start, 0, 600);
    let appended: Vec<Row> = [daily(3, start, 600, 2_500), daily(5, start, 600, 300), daily(8, start, 600, 2_300)].concat();
    let va = {
        let mut a = Store::open(&dir).unwrap();
        let va = a.ingest(REL, &[close_rows(&base)], "test", hot()).unwrap();
        a.ingest(REL, &[close_rows(&appended)], "test", hot()).unwrap();
        assert_eq!(sealed_and_tails(&a, &va), (4, 4, BTreeMap::from([(3, 452), (5, 300), (8, 252)])));
        va
    };
    b.ingest(REL, &[close_rows(&base)], "test", BTreeMap::new()).unwrap();
    let vb = b.ingest(REL, &[close_rows(&appended)], "test", BTreeMap::new()).unwrap();
    // The journal as the defect left it: cut at its first record.
    let vdir = dir.join(format!("hot/{}/{}", REL, va));
    std::fs::write(Overlay::journal_file(&vdir, 0), b"").unwrap();
    let mut a = Store::open(&dir).unwrap();
    assert_eq!(
        sealed_and_tails(&a, &va),
        (0, 0, BTreeMap::from([(3, 2_500), (5, 300), (8, 2_300)])),
        "every appended row is back in a tail"
    );
    same_reads(&a, &va, &b, &vb, &mut rng, &ids, start, 3_100);
    let (v, done) = a.consolidate(REL).unwrap().expect("the due tails sealed, there is something to fold");
    assert_eq!(v, va, "a consolidation keeps the version (ST-8.7)");
    assert_eq!(done.folded_rows, 4 * 1_024);
    assert_eq!(sealed_and_tails(&a, &va), (0, 0, BTreeMap::from([(3, 452), (5, 300), (8, 252)])), "after the consolidation");
    same_reads(&a, &va, &b, &vb, &mut rng, &ids, start, 3_100);
    drop(a);
    let a = Store::open(&dir).unwrap();
    assert_eq!(
        sealed_and_tails(&a, &va),
        (0, 0, BTreeMap::from([(3, 452), (5, 300), (8, 252)])),
        "after the consolidation and a restart"
    );
    same_reads(&a, &va, &b, &vb, &mut rng, &ids, start, 3_100);
    drop(a);
    let _ = std::fs::remove_dir_all(&dir);
}

/// ERRATA E-173: a consolidation asked to folds the tails too. Every row
/// is then in the base, an instrument's last block the partial one
/// ST-4.1 allows; nothing is sealed, no tail is held, and the replay
/// starts past the last row, so a restart reads nothing of the log. Every
/// read answers as before, at every sequence; the appends go on under the
/// version; a second fold re-encodes each partial block with the rows
/// that followed it; a row that arrives inside a folded span is a
/// revision.
#[test]
fn a_consolidation_with_the_tails_leaves_nothing_to_replay() {
    let mut rng = Rng(173);
    let ids = [3u64, 5, 8, 9];
    let start = 18_000i64;
    let dir = persistent("e173");
    let mut b = Store::temporary("e173-twin").unwrap();
    // Published: 3 ends in a partial block, 5 in a full one. Appended: 3
    // seals two blocks and keeps a tail, 5 a tail alone after its full
    // block, 8 (new) two blocks and a tail, 9 (new) ten rows.
    let base: Vec<Row> = [daily(3, start, 0, 600), daily(5, start, 0, 1_024)].concat();
    let appended: Vec<Row> = [daily(3, start, 600, 2_500), daily(5, start, 1_024, 300), daily(8, start, 600, 2_300), daily(9, start, 600, 10)].concat();
    let mut a = Store::open(&dir).unwrap();
    let va = a.ingest(REL, &[close_rows(&base)], "test", hot()).unwrap();
    assert_eq!(a.ingest(REL, &[close_rows(&appended)], "test", hot()).unwrap(), va);
    b.ingest(REL, &[close_rows(&base)], "test", BTreeMap::new()).unwrap();
    let mut vb = b.ingest(REL, &[close_rows(&appended)], "test", BTreeMap::new()).unwrap();
    assert_eq!(sealed_and_tails(&a, &va), (4, 4, BTreeMap::from([(3, 452), (5, 300), (8, 252), (9, 10)])));
    let high = seq_high(&a, &va);
    let filters: Vec<RowFilter> = vec![
        RowFilter::default(),
        RowFilter {
            identity: Some(vec![5, 9]),
            ..RowFilter::default()
        },
        RowFilter {
            latest_only: true,
            ..RowFilter::default()
        },
    ];
    let answers = |s: &Store| -> Vec<String> {
        let mut out = Vec::new();
        for f in &filters {
            for seq in [None, Some(high), Some(high - 5), Some(high - 2_000), Some(high / 2)] {
                out.push(read_text(s, &va, f, seq));
            }
        }
        out
    };
    let before = answers(&a);
    let full_but_the_last = |s: &Store, what: &str| {
        let h = s.hot(&va).unwrap();
        for (o, inst) in h.presence().instruments.iter().enumerate() {
            let n = h.column(0, o).blocks_len();
            assert_eq!(n as u64, inst.rows.div_ceil(abt_codec::BLOCK as u64), "{}: instrument {}'s blocks", what, inst.id);
            for k in 0..n.saturating_sub(1) {
                assert_eq!(h.block(0, o, k).ints.len(), abt_codec::BLOCK, "{}: instrument {} block {}", what, inst.id, k);
            }
        }
    };
    let (v, done) = a.consolidate_with_tails(REL).unwrap().expect("blocks and tails to fold");
    assert_eq!(v, va, "the version id survives (Proposition 7.1)");
    assert_eq!(done.folded_rows, appended.len() as u64, "every appended row is folded");
    assert_eq!(sealed_and_tails(&a, &va), (0, 0, BTreeMap::from([(3, 0), (5, 0), (8, 0), (9, 0)])), "nothing sealed, no tail");
    assert_eq!(a.hot(&va).unwrap().manifest.replay_from, Some(high + 1), "the replay starts past the last row");
    assert_eq!(a.hot(&va).unwrap().presence().rows(), (base.len() + appended.len()) as u64);
    full_but_the_last(&a, "after the fold");
    for (i, (x, y)) in answers(&a).iter().zip(&before).enumerate() {
        assert_same(&format!("answer {} after the fold", i), x, y);
    }
    same_reads(&a, &va, &b, &vb, &mut rng, &ids, start, 3_100);
    assert!(a.consolidate_with_tails(REL).unwrap().is_none(), "nothing more to fold");
    // A restart: no tail comes back from the log.
    drop(a);
    let mut a = Store::open(&dir).unwrap();
    assert_eq!(sealed_and_tails(&a, &va), (0, 0, BTreeMap::from([(3, 0), (5, 0), (8, 0), (9, 0)])), "after a restart");
    for (i, (x, y)) in answers(&a).iter().zip(&before).enumerate() {
        assert_same(&format!("answer {} after the fold and a restart", i), x, y);
    }
    // The appends go on under the version, after partial blocks now.
    let more: Vec<Row> = [daily(3, start, 3_100, 700), daily(5, start, 1_324, 40), daily(8, start, 2_900, 1_100), daily(9, start, 610, 5)].concat();
    assert_eq!(a.ingest(REL, &[close_rows(&more)], "test", hot()).unwrap(), va, "an append after the fold keeps the version");
    vb = b.ingest(REL, &[close_rows(&more)], "test", BTreeMap::new()).unwrap();
    same_reads(&a, &va, &b, &vb, &mut rng, &ids, start, 4_000);
    // A second fold: each partial block re-encoded with what followed it.
    let (v, done) = a.consolidate_with_tails(REL).unwrap().expect("the later rows to fold");
    assert_eq!(v, va);
    assert_eq!(done.folded_rows, more.len() as u64);
    assert_eq!(sealed_and_tails(&a, &va), (0, 0, BTreeMap::from([(3, 0), (5, 0), (8, 0), (9, 0)])));
    full_but_the_last(&a, "after the second fold");
    same_reads(&a, &va, &b, &vb, &mut rng, &ids, start, 4_000);
    drop(a);
    let mut a = Store::open(&dir).unwrap();
    same_reads(&a, &va, &b, &vb, &mut rng, &ids, start, 4_000);
    // A row inside a folded span is a revision: a version (ST-8.5 inserts
    // it only while its span is a tail).
    let late = (9u64, (start + 612) * NANOS_PER_DAY + NANOS_PER_HOUR, (start + 4_100) * NANOS_PER_DAY, 77.25);
    let va2 = a.ingest(REL, &[close_rows(&[late])], "test", hot()).unwrap();
    assert_ne!(va2, va, "a row inside a folded span builds a version");
    let vb2 = b.ingest(REL, &[close_rows(&[late])], "test", BTreeMap::new()).unwrap();
    same_reads(&a, &va2, &b, &vb2, &mut rng, &ids, start, 4_000);
    drop(a);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The length of the well-formed records at the start of a journal.
fn journaled_len(bytes: &[u8]) -> u64 {
    let mut at = 0usize;
    while at + 8 <= bytes.len() && &bytes[at..at + 4] == b"ABTJ" {
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        if at + 8 + len + 32 > bytes.len() {
            break;
        }
        at += 8 + len + 32;
    }
    at as u64
}

#[test]
fn consolidation_keeps_every_answer_and_the_version_id() {
    let mut rng = Rng(14);
    let ids = [2u64, 7, 9, 11];
    let (start, days) = (18_000i64, 4_000i64);
    let rows = market(&mut rng, &ids, start, days);
    let (base, day_batches) = batches(&rows, start, 1_100, days, 15);
    let mut a = Store::temporary("consolidate").unwrap();
    let mut b = Store::temporary("consolidate-twin").unwrap();
    let va = a.ingest(REL, &[close_rows(&base)], "test", hot()).unwrap();
    let mut vb = b.ingest(REL, &[close_rows(&base)], "test", BTreeMap::new()).unwrap();
    // A new instrument between the stored ones, appended only.
    let newcomer = market(&mut rng, &[8], start + 1_100, 2_600);
    for (i, batch) in day_batches.iter().enumerate() {
        a.ingest(REL, &[close_rows(batch)], "test", hot()).unwrap();
        vb = b.ingest(REL, &[close_rows(batch)], "test", BTreeMap::new()).unwrap();
        if i == 10 {
            a.ingest(REL, &[close_rows(&newcomer)], "test", hot()).unwrap();
            vb = b.ingest(REL, &[close_rows(&newcomer)], "test", BTreeMap::new()).unwrap();
        }
    }
    let all_ids = [2u64, 7, 8, 9, 11];
    // Before: the answers, the appended blocks' hashes, the high-water.
    let high = seq_high(&a, &va);
    let filters: Vec<RowFilter> = vec![
        RowFilter::default(),
        RowFilter {
            identity: Some(vec![8, 9]),
            ..RowFilter::default()
        },
        RowFilter {
            t0: Some((start + 900) * NANOS_PER_DAY),
            t1: Some((start + 2_700) * NANOS_PER_DAY),
            latest_only: true,
            ..RowFilter::default()
        },
    ];
    let answers = |s: &Store| -> Vec<String> {
        let mut out = Vec::new();
        for f in &filters {
            for seq in [None, Some(high), Some(high - 800), Some(high / 3)] {
                out.push(read_text(s, &va, f, seq));
            }
        }
        out.push(text(&[s.read_all(&va).unwrap()]));
        out
    };
    let before = answers(&a);
    let (sealed_rows, old_hashes, tails_before) = {
        let h = a.hot(&va).unwrap();
        let mut ov = h.overlay.lock().unwrap();
        let sealed = abt_data::hot::consolidate::sealed_rows(&mut ov).unwrap();
        let mut hashes = Vec::new();
        for o in 0..ov.instruments.len() {
            let blocks = ov.instruments[o].sealed.clone();
            // A sealed block is copied byte for byte when it follows full
            // published blocks; after a partial one it is re-encoded with
            // it (A-D51), and the answers' equality checks it.
            let aligned = ov.instruments[o].published_rows.is_multiple_of(abt_codec::BLOCK as u64);
            for s in &blocks {
                let bytes = ov.bytes_at(s.locs[0]).unwrap();
                hashes.push((ov.instruments[o].id, abt_data::hot::append::block_hash_of(&bytes), aligned));
            }
        }
        let tails: u64 = ov.instruments.iter().map(|i| i.tail.len() as u64).sum();
        (sealed, hashes, tails)
    };
    assert!(sealed_rows.len() >= 2_048, "{} rows sealed", sealed_rows.len());
    let (version, done) = a.consolidate(REL).unwrap().expect("something to fold");
    assert_eq!(version, va, "the version id survives consolidation (Proposition 7.1)");
    assert_eq!(done.folded_rows, sealed_rows.len() as u64);
    assert_eq!(done.manifest.generation, 1);
    // After: the same answers, the twin still agrees, the tails stand.
    let after = answers(&a);
    for (i, (x, y)) in after.iter().zip(&before).enumerate() {
        assert_same(&format!("answer {} after consolidation", i), x, y);
    }
    same_reads(&a, &va, &b, &vb, &mut rng, &all_ids, start, days);
    let h = a.hot(&va).unwrap();
    {
        let ov = h.overlay.lock().unwrap();
        assert!(ov.instruments.iter().all(|i| i.sealed.is_empty()), "nothing sealed remains");
        assert_eq!(ov.instruments.iter().map(|i| i.tail.len() as u64).sum::<u64>(), tails_before, "the tails are rebuilt from the log");
        assert_eq!(ov.generation, 1);
        assert_eq!(ov.journal_records, 0);
    }
    assert_eq!(h.manifest.replay_from, Some(done.manifest.replay_from.unwrap()));
    assert!(h.manifest.replay_from.unwrap() <= high);
    // The folded blocks hash as the sealed ones did, in the base now.
    let p = h.presence();
    let (mut found, mut carried) = (0, 0);
    for (id, hash, aligned) in &old_hashes {
        let o = p.ordinal(*id).unwrap() as usize;
        let n = h.column(0, o).blocks_len();
        let present = (0..n).any(|b| h.block_hash(0, o, b) == *hash);
        if *aligned {
            assert!(present, "instrument {}'s folded block is in the base with its hash", id);
            carried += 1;
        }
        found += 1;
    }
    assert_eq!(found, old_hashes.len());
    assert!(carried >= 1, "the appended-only instrument's blocks carry their hashes");
    // Every block but an instrument's last is full (ST-4.1).
    for (o, inst) in p.instruments.iter().enumerate() {
        let n = h.column(0, o).blocks_len();
        for b in 0..n.saturating_sub(1) {
            assert_eq!(h.block(0, o, b).ints.len(), abt_codec::BLOCK, "instrument {} block {}", inst.id, b);
        }
    }
    // The sealed rows are published rows now: the presence holds every
    // one at its label with its sequence number.
    for (id, label, seq) in &sealed_rows {
        let o = p.ordinal(*id).unwrap() as usize;
        let inst = &p.instruments[o];
        let li = p.labels.partition_point(|l| l < label) as u32;
        assert_eq!(p.labels[li as usize], *label);
        let (row, present) = inst.row_of(li);
        assert!(present);
        assert_eq!(inst.seq_of(row), *seq);
    }
    // The old journal and append directory are gone; the version's
    // directory holds the new presence and manifest.
    let vdir = a.root().join(format!("hot/{}/{}", REL, va));
    assert!(!Overlay::journal_file(&vdir, 0).exists());
    assert!(!Overlay::append_dir(&vdir, 0).exists());
    assert!(vdir.join("presence-g1.bin").exists());
    assert!(!vdir.join("presence.bin").exists(), "the presence the consolidation replaced is gone (E-172)");
    drop(h);
    // Nothing more to fold now; appends go on, a second consolidation
    // folds them, a revision afterwards builds on the folded base.
    assert!(a.consolidate(REL).unwrap().is_none());
    let more = market(&mut rng, &all_ids, start + days, 1_200);
    let (_, later) = batches(&more, start + days, 0, 1_200, 30);
    for batch in &later {
        a.ingest(REL, &[close_rows(batch)], "test", hot()).unwrap();
        vb = b.ingest(REL, &[close_rows(batch)], "test", BTreeMap::new()).unwrap();
    }
    same_reads(&a, &va, &b, &vb, &mut rng, &all_ids, start, days + 1_200);
    let (v2, done2) = a.consolidate(REL).unwrap().expect("something to fold");
    assert_eq!(v2, va);
    assert_eq!(done2.manifest.generation, 2);
    same_reads(&a, &va, &b, &vb, &mut rng, &all_ids, start, days + 1_200);
    // One presence of the base level stands: the last consolidation's.
    let presences = |dir: &std::path::Path| -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .filter(|n| n == "presence.bin" || n.starts_with("presence-g"))
            .collect();
        names.sort();
        names
    };
    assert_eq!(presences(&vdir), ["presence-g2.bin"], "the presences of earlier generations are gone (E-172)");
    let revised = (ids[0], rows[50].1, rows[50].2 + 9 * NANOS_PER_DAY, 9.99);
    let va3 = a.ingest(REL, &[close_rows(&[revised])], "test", hot()).unwrap();
    assert_ne!(va3, va);
    let vb3 = b.ingest(REL, &[close_rows(&[revised])], "test", BTreeMap::new()).unwrap();
    same_reads(&a, &va3, &b, &vb3, &mut rng, &all_ids, start, days + 1_200);
}

#[test]
fn appends_consolidate_on_their_own_past_the_limit() {
    let mut rng = Rng(15);
    let ids = [1u64, 2];
    let (start, days) = (18_000i64, 4_000i64);
    let rows = market(&mut rng, &ids, start, days);
    let (base, day_batches) = batches(&rows, start, 1_100, days, 50);
    let mut a = Store::temporary("auto-consolidate").unwrap();
    a.set_consolidate_bytes(1);
    let va = a.ingest(REL, &[close_rows(&base)], "test", hot()).unwrap();
    let mut generations = 0;
    for batch in &day_batches {
        a.ingest(REL, &[close_rows(batch)], "test", hot()).unwrap();
        let h = a.hot(&va).unwrap();
        let ov = h.overlay.lock().unwrap();
        assert!(ov.instruments.iter().all(|i| i.sealed.is_empty()), "every seal is folded at once under a one-byte limit");
        generations = generations.max(ov.generation);
    }
    assert!(generations >= 2, "{} generations", generations);
    let mut b = Store::temporary("auto-twin").unwrap();
    let mut vb = b.ingest(REL, &[close_rows(&base)], "test", BTreeMap::new()).unwrap();
    for batch in &day_batches {
        vb = b.ingest(REL, &[close_rows(batch)], "test", BTreeMap::new()).unwrap();
    }
    same_reads(&a, &va, &b, &vb, &mut rng, &ids, start, days);
}

#[test]
fn the_map_request_describes_what_a_reader_decodes_with_the_codec() {
    use abt_data::service::{inproc, DataService};
    use abt_wire::generated::abt::wire::{Layer, MapRequest, Message, View};
    use abt_wire::rpc::Client;
    let mut rng = Rng(16);
    let ids = [5u64, 6, 7];
    let (start, days) = (18_000i64, 4_000i64);
    let rows = market(&mut rng, &ids, start, days);
    let (base, day_batches) = batches(&rows, start, 1_100, days, 25);
    let mut a = Store::temporary("map").unwrap();
    let va = a.ingest(REL, &[close_rows(&base)], "test", hot()).unwrap();
    for batch in &day_batches {
        a.ingest(REL, &[close_rows(batch)], "test", hot()).unwrap();
    }
    let high = seq_high(&a, &va);
    let root = a.root().to_path_buf();
    // At the store: segments of the asked column, append files, tails.
    let table = a.map(&ResolvedView::default(), REL, Some(&["P".to_string()]), None, 0, None).unwrap();
    let kinds: Vec<&str> = (0..table.num_rows()).map(|i| table.column_by_name("kind").unwrap().as_string::<i32>().value(i)).collect();
    assert!(kinds.contains(&"segment") && kinds.contains(&"append") && kinds.contains(&"tail"), "{:?}", kinds);
    // Through the service, at a view with a sequence: the tails' visible
    // counts are at that sequence.
    let svc = DataService::new(a);
    let c = Client::connect(inproc(&svc), Layer::Kernel, "test").unwrap();
    let ask = |seq: Option<u64>, from: u64, to: Option<u64>| -> RecordBatch {
        let (_, batches) = c
            .request_table(Message::MapRequest(Box::new(MapRequest {
                view: Some(Box::new(View {
                    versions: None,
                    binding: None,
                    as_of_seq: seq,
                })),
                relation: REL.into(),
                columns: Some(vec!["P".into()]),
                level: None,
                from_entity: from,
                to_entity: to,
            })))
            .unwrap();
        arrow::compute::concat_batches(&batches[0].schema(), &batches).unwrap()
    };
    let at = |b: &RecordBatch, i: usize, name: &str| -> String {
        let c = b.column_by_name(name).unwrap();
        match c.data_type() {
            arrow::datatypes::DataType::Utf8 => c.as_string::<i32>().value(i).to_string(),
            arrow::datatypes::DataType::UInt64 => c.as_primitive::<arrow::datatypes::UInt64Type>().value(i).to_string(),
            arrow::datatypes::DataType::UInt32 => c.as_primitive::<arrow::datatypes::UInt32Type>().value(i).to_string(),
            _ => unreachable!(),
        }
    };
    let full = ask(None, 0, None);
    let early = ask(Some(high - 2), 0, None);
    let tails = |b: &RecordBatch| -> Vec<(u64, u64)> {
        (0..b.num_rows())
            .filter(|&i| at(b, i, "kind") == "tail")
            .map(|i| (at(b, i, "instrument").parse().unwrap(), at(b, i, "rows").parse().unwrap()))
            .collect()
    };
    let (tf, te) = (tails(&full), tails(&early));
    assert!(!tf.is_empty());
    assert_eq!(tf.iter().map(|t| t.0).collect::<Vec<_>>(), te.iter().map(|t| t.0).collect::<Vec<_>>());
    assert!(tf.iter().zip(&te).all(|(f, e)| f.1 >= e.1) && tf.iter().zip(&te).any(|(f, e)| f.1 > e.1), "{:?} {:?}", tf, te);
    // The instrument range narrows the tails and the segments.
    let one = ask(None, 6, Some(6));
    assert_eq!(tails(&one).iter().map(|t| t.0).collect::<Vec<_>>(), vec![6]);
    // A mapped segment, decoded with the codec, reads what a query reads:
    // instrument 5's first block of `P` against the store's rows.
    let seg_rows: Vec<usize> = (0..full.num_rows()).filter(|&i| at(&full, i, "kind") == "segment").collect();
    assert!(!seg_rows.is_empty());
    let file = at(&full, seg_rows[0], "path");
    let hash = at(&full, seg_rows[0], "hash");
    let bytes = std::fs::read(root.join(&file)).unwrap();
    assert_eq!(bytes.len() as u64, at(&full, seg_rows[0], "length").parse::<u64>().unwrap());
    assert_eq!(at(&full, seg_rows[0], "codec").parse::<u32>().unwrap(), abt_codec::CODEC_VERSION as u32);
    assert_eq!(
        blake3::hash(&bytes[..bytes.len() - 40]).to_hex().to_string(),
        hash,
        "the segment's hash is BLAKE3 over its bytes before the trailer's hash"
    );
    let seg = Segment::parse(&bytes, true).unwrap();
    let st = svc.store();
    let h = st.hot(&va).unwrap();
    let run = seg.runs.iter().find(|r| r.first_block == 0).unwrap();
    let inst = &h.presence().instruments[run.instrument as usize];
    let (hd, packed, exceptions) = seg.packed(run, 0);
    let mut out = [0u64; abt_codec::BLOCK];
    abt_codec::block::unpack(&packed, &mut out);
    abt_codec::block::patch(&exceptions, 0, abt_codec::BLOCK, &mut out);
    let decoded: Vec<f64> = (0..hd.n as usize)
        .map(|j| abt_codec::tick::decode_value(hd.base, hd.g, hd.e as u8, hd.recip_exact(), hd.f32(), out[j]))
        .collect();
    let f = RowFilter {
        identity: Some(vec![inst.id]),
        ..RowFilter::default()
    };
    let queried = st.read(&va, &f).unwrap();
    let col = queried[0].column_by_name("P").unwrap().as_primitive::<arrow::datatypes::Float64Type>();
    let first: Vec<f64> = (0..decoded.len()).map(|i| col.value(i)).collect();
    assert_eq!(decoded, first, "the mapped block decodes to the queried rows (ST-10.3)");
    // A relation held in Parquet has nothing to map.
    let err = st.map(&ResolvedView::default(), "equity.volume", None, None, 0, None).err();
    assert!(err.is_some());
}

#[test]
fn stats_carry_the_hot_figures_and_the_process_row() {
    use abt_data::service::{inproc, DataService};
    use abt_wire::generated::abt::wire::{Layer, Message, StatsRequest};
    use abt_wire::rpc::Client;
    let mut rng = Rng(17);
    let rows = market(&mut rng, &[1, 2], 18_000, 4_000);
    let (base, day_batches) = batches(&rows, 18_000, 1_100, 4_000, 100);
    let mut a = Store::temporary("stats").unwrap();
    let va = a.ingest(REL, &[close_rows(&base)], "test", hot()).unwrap();
    for batch in &day_batches {
        a.ingest(REL, &[close_rows(batch)], "test", hot()).unwrap();
    }
    let figures = a.hot_figures(&va).unwrap();
    assert!(
        figures.segment_bytes > 0 && figures.append_bytes > 0 && figures.journal_records > 0 && figures.tail_rows > 0,
        "{:?}",
        figures
    );
    let svc = DataService::new(a);
    svc.set_restart_millis(1_234);
    let c = Client::connect(inproc(&svc), Layer::Kernel, "test").unwrap();
    let (_, batches) = c.request_table(Message::StatsRequest(Box::new(StatsRequest { relation: None }))).unwrap();
    let b = arrow::compute::concat_batches(&batches[0].schema(), &batches).unwrap();
    let rel = b.column_by_name("relation").unwrap().as_string::<i32>();
    let u = |name: &str, i: usize| b.column_by_name(name).unwrap().as_primitive::<arrow::datatypes::UInt64Type>().value(i);
    let hot_row = (0..b.num_rows()).find(|&i| rel.value(i) == REL).unwrap();
    assert_eq!(b.column_by_name("storage").unwrap().as_string::<i32>().value(hot_row), "hot");
    assert_eq!(u("hot_bytes", hot_row), figures.segment_bytes);
    assert_eq!(u("tail_rows", hot_row), figures.tail_rows);
    assert_eq!(u("journal_records", hot_row), figures.journal_records);
    let process = (0..b.num_rows()).find(|&i| rel.value(i) == "_data").expect("the process row");
    assert_eq!(u("restart_millis", process), 1_234);
    assert_eq!(b.column_by_name("profile").unwrap().as_string::<i32>().value(process), "workstation");
    assert_eq!(b.column_by_name("kernel_limit").unwrap().as_primitive::<arrow::datatypes::UInt32Type>().value(process), 2);
    // Asked for one relation, the process row stays out.
    let (_, batches) = c.request_table(Message::StatsRequest(Box::new(StatsRequest { relation: Some(REL.into()) }))).unwrap();
    let b = arrow::compute::concat_batches(&batches[0].schema(), &batches).unwrap();
    let rel = b.column_by_name("relation").unwrap().as_string::<i32>();
    assert!((0..b.num_rows()).all(|i| rel.value(i) == REL));
}

/// E-121: an instrument appended since publication as a new one lives in
/// the overlay alone; an ingest that is no append (a revision of another
/// instrument, the retry of an ingest killed mid-write) stages a version
/// from the published instruments and the input, and dropped it with every
/// row it held. A futures store lost every root ingested after the first
/// from `high` and `low` this way (17.8 million rows of 102.4). Every
/// instrument's rows survive, and the version reads as its Parquet twin.
#[test]
fn a_rebuild_keeps_the_instruments_appended_since_publication() {
    let day = |d: i64| (19_000 + d) * NANOS_PER_DAY;
    let first: Vec<(u64, i64, i64, f64)> = (0..20).map(|d| (1u64, day(d), day(d), 10.5 + d as f64)).collect();
    let second: Vec<(u64, i64, i64, f64)> = (0..20).map(|d| (2u64, day(d), day(d), 20.25 + d as f64)).collect();
    let revision = vec![(1u64, day(3), day(25), 99.5)];
    let mut a = Store::temporary("rebuild-overlay").unwrap();
    let mut b = Store::temporary("rebuild-twin").unwrap();
    let v1 = a.ingest(REL, &[close_rows(&first)], "test", hot()).unwrap();
    b.ingest(REL, &[close_rows(&first)], "test", BTreeMap::new()).unwrap();
    // A new instrument: an append, under the published version.
    assert_eq!(a.ingest(REL, &[close_rows(&second)], "test", hot()).unwrap(), v1, "an append keeps the version");
    b.ingest(REL, &[close_rows(&second)], "test", BTreeMap::new()).unwrap();
    // A revision: no append, a new version.
    let va = a.ingest(REL, &[close_rows(&revision)], "test", hot()).unwrap();
    let vb = b.ingest(REL, &[close_rows(&revision)], "test", BTreeMap::new()).unwrap();
    assert_ne!(va, v1, "a new version");
    let rows_of = |id: u64| {
        a.read(
            &va,
            &RowFilter {
                identity: Some(vec![id]),
                latest_only: true,
                ..RowFilter::default()
            },
        )
        .unwrap()
        .iter()
        .map(|b| b.num_rows())
        .sum::<usize>()
    };
    assert_eq!((rows_of(1), rows_of(2)), (20, 20), "every instrument's rows");
    same_reads(&a, &va, &b, &vb, &mut Rng(4), &[1, 2], 19_000, 26);
}
