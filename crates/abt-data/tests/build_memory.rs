//! E-122: an ingest builds the instruments it writes; the others' blocks,
//! at every level, are the previous version's, and its memory follows what
//! it writes, not the store. S2 read every stored instrument's rows and S3
//! rebuilt every instrument's ladder from them at once: a futures store's
//! ingests grew 0.21 GB per million stored rows (29 GB at 137 million). One
//! test in its own binary: it reads the process's counters and memory.

mod hot_common;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use abt_codec::summary::Column;
use abt_data::staging::classify::rows_read;
use abt_data::Store;
use abt_wire::scalar::{NANOS_PER_DAY, NANOS_PER_HOUR};
use hot_common::*;

const REL: &str = "equity.close";
const START: i64 = 18_000;
const DAYS: i64 = 6_000;

struct Ingested {
    store: Store,
    before: String,
    after: String,
    /// Instrument 1's rows in the version before.
    revised_rows: u64,
    /// The stored rows the ingest read back.
    read: u64,
    /// The most anonymous memory the ingest added.
    grew: u64,
}

/// A store of `n` instruments over `DAYS` days, then an ingest of a new
/// instrument on days every stored one has, with a revision of instrument
/// 1 (no append: a version is built).
fn ingest_into(n: u64) -> Ingested {
    let mut rng = Rng(n);
    let ids: Vec<u64> = (1..=n).collect();
    let rows = market(&mut rng, &ids, START, DAYS);
    let mut store = Store::temporary(&format!("build-memory-{}", n)).unwrap();
    let before = store.ingest(REL, &[close_rows(&rows)], "test", hot()).unwrap();
    let revised_rows = rows.iter().filter(|r| r.0 == 1).count() as u64;
    // Every instrument is alive over the middle half (`market`), the days
    // there each some instrument's: the new rows add no label.
    let mut more: Vec<(u64, i64, i64, f64)> = (DAYS / 4 + 1..DAYS * 3 / 4 - 1)
        .map(|d| {
            let t = (START + d) * NANOS_PER_DAY;
            (n + 1, t, t + NANOS_PER_HOUR, 50.0 + (d % 97) as f64 / 4.0)
        })
        .collect();
    let first = rows.iter().find(|r| r.0 == 1).unwrap();
    more.push((1, first.1, first.2 + NANOS_PER_DAY, first.3 + 1.0));
    more.sort_by_key(|r| (r.1, r.0));
    let input = close_rows(&more);
    abt_wire::memory::trim_now();
    let base = abt_wire::memory::anonymous_bytes().unwrap_or(0);
    let peak = Arc::new(AtomicU64::new(base));
    let running = Arc::new(AtomicBool::new(true));
    let sampler = {
        let (peak, running) = (peak.clone(), running.clone());
        std::thread::spawn(move || {
            while running.load(Ordering::Relaxed) {
                peak.fetch_max(abt_wire::memory::anonymous_bytes().unwrap_or(0), Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(2));
            }
        })
    };
    let read_before = rows_read();
    let after = store.ingest(REL, &[input], "test", hot()).unwrap();
    let read = rows_read() - read_before;
    running.store(false, Ordering::Relaxed);
    sampler.join().unwrap();
    Ingested {
        store,
        before,
        after,
        revised_rows,
        read,
        grew: peak.load(Ordering::Relaxed).saturating_sub(base),
    }
}

#[test]
fn an_ingest_builds_what_it_writes_and_its_memory_follows_it() {
    let small = ingest_into(6);
    let small = (small.read, small.revised_rows, small.grew);
    let large = ingest_into(96);
    eprintln!(
        "an ingest into 6 instruments read {} stored rows and added {} MB; into 96, {} rows and {} MB",
        small.0,
        small.2 >> 20,
        large.read,
        large.grew >> 20
    );
    assert_ne!(large.before, large.after, "a version was built");
    // It read back the instrument it revised, and no other.
    assert_eq!(small.0, small.1);
    assert_eq!(large.read, large.revised_rows);
    // The kept instruments' blocks, at every level, are the version's before.
    let (h1, h2) = (large.store.hot(&large.before).unwrap(), large.store.hot(&large.after).unwrap());
    let mut compared = 0;
    for id in 2..=96u64 {
        for (level, lv) in h2.levels.iter().enumerate() {
            let (a, b) = (h1.levels[level].presence.ordinal(id).unwrap() as usize, lv.presence.ordinal(id).unwrap() as usize);
            for c in 0..lv.columns.len() {
                let n = h2.column_at(level, c, b).blocks();
                assert_eq!(n, h1.column_at(level, c, a).blocks());
                for k in 0..n {
                    assert_eq!(
                        h2.block_hash_at(level, c, b, k),
                        h1.block_hash_at(level, c, a, k),
                        "instrument {} level {} column {} block {}",
                        id,
                        level,
                        c,
                        k
                    );
                    compared += 1;
                }
            }
        }
    }
    assert!(compared >= 95 * 3 * 2, "{} blocks compared", compared);
    // Sixteen times the store, not sixteen times the memory (reading every
    // stored instrument back added some 100 MB at 96 instruments).
    assert!(large.grew <= small.2 + (32 << 20), "into 96 instruments {} MB, into 6 {} MB", large.grew >> 20, small.2 >> 20);
}
