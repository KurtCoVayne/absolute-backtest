//! Streams (D-5): barriers partition the stream by receipt time
//! (Proposition D-5.2); a subscription from any `from_seq` replays the same
//! events with the same `seq` (Proposition D-5.4); the barrier index gives
//! each barrier's `seq` without streaming; a row received in a later
//! partition bucket than its event time is emitted in receipt order (E-14);
//! barriers of one instant follow the spec's calendar order (E-13); and the
//! merge equals a naive sort of every event over random stores.

use std::collections::BTreeMap;
use std::sync::Arc;

use abt_data::stream::{index, resolve, Stream};
use abt_data::Store;
use abt_wire::generated::abt::wire::{ReceiptFilter, StreamSpec, VersionRef};
use arrow::array::{Array, AsArray, Float64Array, RecordBatch, StringArray, TimestampNanosecondArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit, UInt32Type};

const DAY: i64 = 86_400 * 1_000_000_000;
const T0: i64 = 19_000 * DAY;

fn close_rows(rows: &[(u64, i64, i64, f64)]) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("A", DataType::UInt64, false),
        Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("P", DataType::Float64, false),
        Field::new("r", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
    ]));
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(UInt64Array::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
            Arc::new(Float64Array::from(rows.iter().map(|r| r.3).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
        ],
    )
    .unwrap()
}

fn calendar_rows(rows: &[(&str, i64, i64)]) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("calendar", DataType::Utf8, false),
        Field::new("label", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("at", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
    ]));
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
        ],
    )
    .unwrap()
}

/// What a test reads of an event.
#[derive(Clone, Debug, PartialEq)]
enum E {
    Tuple { seq: u64, entity: u64, t: i64, r: i64, p: f64 },
    Barrier { seq: u64, calendar: String, label: i64, at: i64 },
}

impl E {
    fn seq(&self) -> u64 {
        match self {
            E::Tuple { seq, .. } | E::Barrier { seq, .. } => *seq,
        }
    }
}

fn events(batches: &[RecordBatch]) -> Vec<E> {
    let mut out = Vec::new();
    for b in batches {
        let col = |n: &str| b.column(b.schema().index_of(n).unwrap()).clone();
        let seq = col("seq");
        let seq = seq.as_primitive::<arrow::datatypes::UInt64Type>();
        let kind = col("kind");
        let kind = kind.as_dictionary::<arrow::datatypes::UInt8Type>();
        let kinds = kind.values().as_string::<i32>();
        let entity = col("entity");
        let entity = entity.as_primitive::<arrow::datatypes::UInt64Type>();
        let t = col("t");
        let t = t.as_primitive::<arrow::datatypes::TimestampNanosecondType>();
        let r = col("r");
        let r = r.as_primitive::<arrow::datatypes::TimestampNanosecondType>();
        let cal = col("calendar");
        let cal = cal.as_dictionary::<UInt32Type>();
        let cals = cal.values().as_string::<i32>();
        let label = col("label");
        let label = label.as_primitive::<arrow::datatypes::TimestampNanosecondType>();
        let at = col("at");
        let at = at.as_primitive::<arrow::datatypes::TimestampNanosecondType>();
        let p = col("equity.close.P");
        let p = p.as_primitive::<arrow::datatypes::Float64Type>();
        for i in 0..b.num_rows() {
            match kinds.value(kind.keys().value(i) as usize) {
                "tuple" => out.push(E::Tuple {
                    seq: seq.value(i),
                    entity: entity.value(i),
                    t: t.value(i),
                    r: r.value(i),
                    p: p.value(i),
                }),
                "barrier" => out.push(E::Barrier {
                    seq: seq.value(i),
                    calendar: cals.value(cal.keys().value(i) as usize).to_string(),
                    label: label.value(i),
                    at: at.value(i),
                }),
                k => panic!("unexpected kind {}", k),
            }
        }
    }
    out
}

fn read(store: &Store, spec: &StreamSpec, from: u64, to: Option<u64>, max: usize) -> Vec<E> {
    let res = resolve(store, spec).unwrap();
    let mut s = Stream::open(store, res).unwrap();
    let mut batches = Vec::new();
    while let Some(b) = s.next_batch(store, from, to, max).unwrap() {
        assert!(b.num_rows() <= max);
        batches.push(b);
    }
    events(&batches)
}

/// Three instruments over five days of January, received at the bar, one
/// close corrected two days later, and one received in February (a later
/// partition bucket than its event time); the daily calendar's barriers at
/// each day's last instant and a weekly calendar's on the last day.
fn store() -> (Store, StreamSpec) {
    let mut s = Store::temporary("stream").unwrap();
    let mut rows = Vec::new();
    for d in 0..5 {
        for a in 1..=3u64 {
            rows.push((a, T0 + d * DAY, T0 + d * DAY, 10.0 * a as f64 + d as f64));
        }
    }
    rows.push((2, T0 + DAY, T0 + 3 * DAY + 5, 99.0));
    rows.push((3, T0 + 2 * DAY, T0 + 40 * DAY, 77.0));
    let close = s.ingest("equity.close", &[close_rows(&rows)], "test", BTreeMap::new()).unwrap();
    let mut cal = Vec::new();
    for d in 0..5 {
        cal.push(("1d", T0 + d * DAY, T0 + d * DAY + DAY - 1));
    }
    cal.push(("1w", T0 + 4 * DAY, T0 + 4 * DAY + DAY - 1));
    cal.push(("1d", T0 + 40 * DAY, T0 + 40 * DAY + DAY - 1));
    s.ingest("calendar_label", &[calendar_rows(&cal)], "test", BTreeMap::new()).unwrap();
    let spec = StreamSpec {
        relations: Some(vec![VersionRef {
            relation: "equity.close".into(),
            version: close,
        }]),
        binding: None,
        calendars: Some(vec!["1d".into(), "1w".into()]),
        receipt_filter: None,
        clock: abt_wire::generated::abt::wire::ClockSource::Replay,
        session: None,
    };
    (s, spec)
}

#[test]
fn barriers_partition_the_stream_by_receipt_time() {
    let (s, spec) = store();
    let ev = read(&s, &spec, 1, None, 1024);
    // Gap-free from 1.
    assert_eq!(ev.iter().map(|e| e.seq()).collect::<Vec<_>>(), (1..=ev.len() as u64).collect::<Vec<_>>());
    // Proposition D-5.2, per calendar: the tuples between two of its barriers
    // were received in (at_prev, at].
    for cal in ["1d", "1w"] {
        let mut prev = i64::MIN;
        let mut pending: Vec<i64> = Vec::new();
        for e in &ev {
            match e {
                E::Tuple { r, .. } => pending.push(*r),
                E::Barrier { calendar, at, .. } if calendar == cal => {
                    assert!(pending.iter().all(|r| *r > prev && *r <= *at), "{} barrier at {}: {:?}", cal, at, pending);
                    pending.clear();
                    prev = *at;
                }
                _ => {}
            }
        }
        assert!(pending.iter().all(|r| *r > prev));
    }
    // The correction is a tuple of its own, after the third day's barrier.
    let corr = ev.iter().position(|e| matches!(e, E::Tuple { p, .. } if *p == 99.0)).unwrap();
    let third = ev
        .iter()
        .position(|e| matches!(e, E::Barrier { calendar, label, .. } if calendar == "1d" && *label == T0 + 2 * DAY))
        .unwrap();
    assert!(corr > third);
    // The February receipt of a January row comes after every January barrier
    // and before February's (E-14).
    let feb = ev.iter().position(|e| matches!(e, E::Tuple { p, .. } if *p == 77.0)).unwrap();
    let last_jan = ev.iter().rposition(|e| matches!(e, E::Barrier { label, .. } if *label == T0 + 4 * DAY)).unwrap();
    assert!(feb > last_jan);
    assert!(matches!(ev.last().unwrap(), E::Barrier { label, .. } if *label == T0 + 40 * DAY));
    // At one instant: the daily barrier, then the weekly (the spec's order).
    let d5 = ev
        .iter()
        .position(|e| matches!(e, E::Barrier { calendar, label, .. } if calendar == "1d" && *label == T0 + 4 * DAY))
        .unwrap();
    assert!(matches!(&ev[d5 + 1], E::Barrier { calendar, .. } if calendar == "1w"));
    // Within an instant, tuples in identity order.
    let first: Vec<u64> = ev.iter().take(3).map(|e| if let E::Tuple { entity, .. } = e { *entity } else { 0 }).collect();
    assert_eq!(first, vec![1, 2, 3]);
}

#[test]
fn a_subscription_replays_from_any_seq_and_stops_at_to_seq() {
    let (s, spec) = store();
    let all = read(&s, &spec, 1, None, 1024);
    for from in [1, 2, 7, 13, all.len() as u64] {
        for max in [1, 3, 1024] {
            let tail = read(&s, &spec, from, None, max);
            assert_eq!(tail, all[(from - 1) as usize..].to_vec(), "from {} in batches of {}", from, max);
        }
    }
    let head = read(&s, &spec, 1, Some(9), 4);
    assert_eq!(head, all[..9].to_vec());
    let mid = read(&s, &spec, 5, Some(9), 2);
    assert_eq!(mid, all[4..9].to_vec());
}

#[test]
fn the_index_gives_each_barrier_its_seq() {
    let (s, spec) = store();
    let all = read(&s, &spec, 1, None, 1024);
    let res = resolve(&s, &spec).unwrap();
    let ix = index(&s, &res).unwrap();
    let barriers: Vec<(String, i64, i64, u64)> = all
        .iter()
        .filter_map(|e| match e {
            E::Barrier { seq, calendar, label, at } => Some((calendar.clone(), *label, *at, *seq)),
            _ => None,
        })
        .collect();
    let from_index: Vec<(String, i64, i64, u64)> = ix.iter().map(|(c, b, s)| (c.clone(), b.label, b.at, *s)).collect();
    assert_eq!(barriers, from_index);
    // Truncating at a barrier's seq is the stream as of that barrier.
    let (_, b, seq) = &ix[2];
    let cut = read(&s, &spec, 1, Some(*seq), 1024);
    assert!(matches!(cut.last().unwrap(), E::Barrier { label, .. } if *label == b.label));
}

#[test]
fn the_receipt_filter_ends_the_stream() {
    let (s, mut spec) = store();
    spec.receipt_filter = Some(Box::new(ReceiptFilter { r_max: Some(T0 + 3 * DAY) }));
    let ev = read(&s, &spec, 1, None, 1024);
    assert!(ev.iter().all(|e| match e {
        E::Tuple { r, .. } => *r <= T0 + 3 * DAY,
        E::Barrier { at, .. } => *at <= T0 + 3 * DAY,
    }));
    assert!(!ev.iter().any(|e| matches!(e, E::Tuple { p, .. } if *p == 99.0)), "received after the filter");
    // Three days of bars, the barriers of the first three days.
    assert_eq!(ev.iter().filter(|e| matches!(e, E::Barrier { .. })).count(), 3);
}

/// A seeded generator (xorshift*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[test]
fn the_merge_is_the_sort_of_every_event() {
    for seed in 1..=12u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let mut s = Store::temporary("stream-random").unwrap();
        // Rows over three months, received up to 50 days late, in several
        // ingests (so partitions are rewritten and revisions accumulate).
        let mut naive: Vec<(i64, u64, i64, u64, f64)> = Vec::new();
        let mut version = String::new();
        let mut seq = 0u64;
        for _ in 0..1 + rng.below(4) {
            let mut rows = Vec::new();
            let mut seen = std::collections::BTreeSet::new();
            for _ in 0..1 + rng.below(60) {
                let a = 1 + rng.below(4);
                let t = T0 + rng.below(90) as i64 * DAY;
                let r = t + rng.below(50) as i64 * DAY / 2;
                if !seen.insert((a, t, r)) || naive.iter().any(|n| n.1 == a && n.2 == t && n.0 == r) {
                    continue;
                }
                let p = rng.below(1000) as f64;
                rows.push((a, t, r, p));
            }
            if rows.is_empty() {
                continue;
            }
            for &(a, t, r, p) in &rows {
                seq += 1;
                naive.push((r, a, t, seq, p));
            }
            version = s.ingest("equity.close", &[close_rows(&rows)], "test", BTreeMap::new()).unwrap();
        }
        if version.is_empty() {
            continue;
        }
        let mut cal = Vec::new();
        for d in (0..140).step_by(1 + rng.below(9) as usize) {
            cal.push(("1d", T0 + d * DAY, T0 + d * DAY + DAY - 1));
        }
        s.ingest("calendar_label", &[calendar_rows(&cal)], "test", BTreeMap::new()).unwrap();
        let spec = StreamSpec {
            relations: Some(vec![VersionRef {
                relation: "equity.close".into(),
                version,
            }]),
            binding: None,
            calendars: Some(vec!["1d".into()]),
            receipt_filter: None,
            clock: abt_wire::generated::abt::wire::ClockSource::Replay,
            session: None,
        };
        // The naive order: every row by (r, entity, t, seq), every barrier by
        // its instant, tuples first.
        let mut want: Vec<(i64, u8, u64, i64, u64, f64)> = naive.iter().map(|&(r, a, t, sq, p)| (r, 0u8, a, t, sq, p)).collect();
        for &(_, l, at) in &cal {
            want.push((at, 2, 0, l, 0, 0.0));
        }
        want.sort_by_key(|x| (x.0, x.1, x.2, x.3, x.4));
        let got = read(&s, &spec, 1, None, 1 + rng.below(40) as usize);
        assert_eq!(got.len(), want.len(), "seed {}", seed);
        for (g, w) in got.iter().zip(&want) {
            match g {
                E::Tuple { entity, t, r, p, .. } => assert_eq!((w.1, *r, *entity, *t, *p), (0, w.0, w.2, w.3, w.5), "seed {}", seed),
                E::Barrier { label, at, .. } => assert_eq!((w.1, *at, *label), (2, w.0, w.3), "seed {}", seed),
            }
        }
        // And the index agrees with the stream.
        let res = resolve(&s, &spec).unwrap();
        let ix: Vec<u64> = index(&s, &res).unwrap().iter().map(|(_, _, sq)| *sq).collect();
        let seqs: Vec<u64> = got.iter().filter(|e| matches!(e, E::Barrier { .. })).map(|e| e.seq()).collect();
        assert_eq!(ix, seqs, "seed {}", seed);
    }
}

#[test]
fn a_stream_without_calendars_carries_tuples_only() {
    let (s, mut spec) = store();
    spec.calendars = None;
    let ev = read(&s, &spec, 1, None, 1024);
    assert_eq!(ev.len(), 17);
    assert!(ev.iter().all(|e| matches!(e, E::Tuple { .. })));
    // A label column of a relation is a dictionary over the registry's labels.
    let res = resolve(&s, &spec).unwrap();
    let st = Stream::open(&s, res).unwrap();
    assert!(st.schema().field_with_name("equity.close.A").is_ok());
    assert!(st.schema().field_with_name("equity.close.T").is_err(), "times are the event's own columns");
    assert!(st.schema().field_with_name("equity.close.P").unwrap().is_nullable());
    let _ = st.schema().field_with_name("relation").unwrap().data_type().clone();
    assert!(!st.id.is_empty());
    let _: &dyn Array = &UInt64Array::from(vec![1u64]);
}
