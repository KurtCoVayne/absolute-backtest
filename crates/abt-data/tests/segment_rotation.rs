//! E-123: a new instrument between two stored ones shifts the kept
//! instruments' ordinals, and their runs are rewritten at the new ones. A
//! segment the rewrite filled and rotated on the way was dropped from the
//! version's lists (they were reset to the rebuilt runs' segments after the
//! rewrite): its instruments lost their blocks, and the version did not
//! open (S4 refused the ingest). At 64 MiB a segment, a store of a few
//! million rows. Its own binary: it sets the segment size of the process.

mod hot_common;

use abt_data::staging::build::set_segment_bytes;
use abt_data::{RowFilter, Store};
use abt_wire::scalar::NANOS_PER_DAY;
use hot_common::*;

#[test]
fn a_rewrite_of_the_kept_runs_past_a_segment_keeps_them_all() {
    // A segment rotates at 4 KiB: every few instruments' runs.
    set_segment_bytes(4 << 10);
    let mut rng = Rng(7);
    let ids: Vec<u64> = (1..=40).map(|i| 2 * i).collect();
    let rows = market(&mut rng, &ids, 18_000, 400);
    let mut s = Store::temporary("segment-rotation").unwrap();
    let v1 = s.ingest("equity.close", &[close_rows(&rows)], "test", hot()).unwrap();
    // Instrument 3 sits between 2 and 4: every ordinal after it shifts. A
    // revision of instrument 2 (the ingest is no append) builds a version.
    let mut more = market(&mut rng, &[3], 18_000, 400);
    let first = rows.iter().find(|r| r.0 == 2).unwrap();
    more.push((2, first.1, first.2 + NANOS_PER_DAY, first.3 + 1.0));
    more.sort_by_key(|r| (r.1, r.0));
    let v2 = s.ingest("equity.close", &[close_rows(&more)], "test", hot()).unwrap();
    assert_ne!(v1, v2);
    let h = s.hot(&v2).unwrap();
    for &id in ids.iter().chain(&[3]) {
        let f = RowFilter {
            identity: Some(vec![id]),
            latest_only: true,
            ..RowFilter::default()
        };
        let want = rows.iter().chain(&more).filter(|r| r.0 == id).count() - (id == 2) as usize;
        assert_eq!(h.read(&f, 0, u32::MAX).unwrap().len(), want, "instrument {}", id);
    }
}
