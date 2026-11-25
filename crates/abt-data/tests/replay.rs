//! E-128: a hot version's replay of the log into its tails keeps the rows
//! after their instruments' sealed labels, batch by batch. After a
//! consolidation the replay starts at the least sequence a tail held
//! (ST-8.8), and the log after it is mostly rows the version seals: a
//! futures store's consolidated `future.close` replayed 126 million rows
//! at each open, held and normalised them all for tails of a few thousand,
//! and an ingest of one root grew past 20 GB. One test in its own binary:
//! it reads the process's counter of the rows replays keep.

mod hot_common;

use abt_data::store::replay_kept;
use abt_data::{RowFilter, Store};
use abt_wire::scalar::{NANOS_PER_DAY, NANOS_PER_HOUR};
use hot_common::*;

const REL: &str = "equity.close";

fn row(id: u64, d: i64, p: f64) -> (u64, i64, i64, f64) {
    let t = (18_000 + d) * NANOS_PER_DAY;
    (id, t, t + NANOS_PER_HOUR, p)
}

#[test]
fn a_replay_after_a_consolidation_keeps_the_tails_rows_alone() {
    let dir = std::env::temp_dir().join(format!("abt-replay-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // Two instruments published over 100 days; then instrument 7 appended
    // three days (a tail early in the log), instrument 2 appended 5,000
    // (sealed into append files, but for its last rows).
    let base: Vec<_> = (0..100).flat_map(|d| [row(2, d, 10.0 + d as f64 / 8.0), row(7, d, 70.0 + d as f64 / 4.0)]).collect();
    let early: Vec<_> = (100..103).map(|d| row(7, d, 90.0 + d as f64 / 4.0)).collect();
    let late: Vec<_> = (100..5_100).map(|d| row(2, d, 20.0 + (d % 400) as f64 / 8.0)).collect();
    let (version, before, tails) = {
        let mut s = Store::open(&dir).unwrap();
        let v = s.ingest(REL, &[close_rows(&base)], "test", hot()).unwrap();
        s.ingest(REL, &[close_rows(&early)], "test", hot()).unwrap();
        for chunk in late.chunks(500) {
            s.ingest(REL, &[close_rows(chunk)], "test", hot()).unwrap();
        }
        let (folded, done) = s.consolidate(REL).unwrap().expect("sealed blocks to fold");
        assert_eq!(folded, v);
        let from = done.manifest.replay_from.expect("a consolidated version replays from its least tail");
        assert!(from <= 203, "the replay starts at instrument 7's tail, sequence {}", from);
        let h = s.hot(&v).unwrap();
        let tails: u64 = h.overlay.lock().unwrap().instruments.iter().map(|i| i.tail.len() as u64).sum();
        let all = text(&[s.read_all(&v).unwrap()]);
        (v, all, tails)
    };
    assert!((3..1_024 + 3).contains(&tails), "{} rows in the tails", tails);
    // A fresh open replays the log from instrument 7's tail: every row of
    // instrument 2 after it is read, and only the tails' are kept.
    let kept = replay_kept();
    let s = Store::open(&dir).unwrap();
    let after = text(&[s.read_all(&version).unwrap()]);
    assert_eq!(before, after, "a restart answers what it answered");
    assert_eq!(replay_kept() - kept, tails, "the replay kept the tails' rows of the {} after its start", late.len());
    let f = RowFilter {
        identity: Some(vec![7]),
        ..RowFilter::default()
    };
    assert_eq!(s.read(&version, &f).unwrap().iter().map(|b| b.num_rows()).sum::<usize>(), 103);
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}
