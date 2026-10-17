//! Live sessions and streams (D-5.2 live, D-5.4, D-5.5) on a synthetic feed
//! and a clock running 120 times the wall clock, the base calendar's grace
//! ten seconds (83 ms of wall time, room for a loaded machine's scheduling):
//!
//! - the session records every bar write-ahead, received at its acceptance
//!   (after its close plus the feed's delay, monotone);
//! - a live stream emits each barrier when the clock passes its label plus
//!   the grace, after the bars accepted before it, and records the instants
//!   as `kernel.<run>.clock`;
//! - the recorded stream replays: the session's versions merged with the
//!   recorded clock give the live stream's events with the same `seq`
//!   (Proposition D-5.4), and a paced replay takes the session's time on the
//!   clock (D-5.5).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use abt_data::live::{clock_rows, clock_schema, Clock, ScaledClock, SyntheticFeed};
use abt_data::resolver::Sources;
use abt_data::service::{inproc, DataService};
use abt_data::Store;
use abt_wire::generated::abt::wire::{ClockSource, Layer, Message, StreamSpec, SubscribeRequest, VersionRef};
use abt_wire::rpc::Client;
use arrow::array::{Array, AsArray, RecordBatch};
use arrow::datatypes::{TimestampNanosecondType, UInt64Type};

const MIN: i64 = 60_000_000_000;
const RATE: f64 = 120.0;
const GRACE: i64 = 10_000_000_000;

fn t(s: &str) -> i64 {
    abt_wire::scalar::parse_timestamp(s).unwrap()
}

/// Six one-minute bars from 14:31, the clock starting a minute before the
/// first, with the session opened and recording.
fn session(tag: &str) -> (Arc<DataService>, Vec<i64>, abt_data::live::Recording) {
    let labels: Vec<i64> = (1..=6).map(|k| t("2024-03-11T14:30:00") + k * MIN).collect();
    let clock: Arc<dyn Clock> = Arc::new(ScaledClock::new(labels[0] - MIN, RATE));
    let grace: BTreeMap<String, i64> = [("base".to_string(), GRACE)].into_iter().collect();
    let svc = DataService::with_clock(Store::temporary(tag).unwrap(), Sources::store_only(), clock.clone(), grace);
    // Each bar arrives 50 ms after its close, well within the grace.
    let feed = SyntheticFeed::new(clock, &["AAA", "BBB"], labels.clone(), 50_000_000, 7);
    svc.open_session("s1", "synthetic", &feed.prelude().unwrap(), &labels).unwrap();
    let h = svc.record("s1", "synthetic", Box::new(feed));
    (svc, labels, h)
}

/// A subscriber's planned clock: the base calendar's labels, put as
/// `kernel.<run>.calendar_label`.
fn planned(svc: &DataService, run: &str, labels: &[i64]) -> VersionRef {
    let name = format!("kernel.{}.calendar_label", run);
    let rows: Vec<(&str, i64, i64)> = labels.iter().map(|&l| ("base", l, l)).collect();
    let (version, _) = svc.store_mut().put(&clock_schema(&name, "kernel"), &[clock_rows(&rows).unwrap()], "put").unwrap();
    VersionRef { relation: name, version }
}

fn events(c: &Client, spec: StreamSpec, paced: bool) -> Vec<RecordBatch> {
    let mut reply = c
        .call(Message::SubscribeRequest(Box::new(SubscribeRequest {
            spec: Box::new(spec),
            from_seq: 0,
            paced,
            to_seq: None,
            with_index: false,
        })))
        .unwrap();
    let Message::SubscribeResponse(_) = reply.message().unwrap() else { panic!("no subscription") };
    let Message::TableFollows(_) = reply.message().unwrap() else { panic!("no events") };
    reply.table().unwrap().1
}

/// `(seq, kind, t or label, r or at)` of every event.
fn rows(batches: &[RecordBatch]) -> Vec<(u64, String, i64, i64)> {
    let mut out = Vec::new();
    for b in batches {
        let seq = b.column_by_name("seq").unwrap().as_primitive::<UInt64Type>();
        let kind = arrow::compute::cast(b.column_by_name("kind").unwrap(), &arrow::datatypes::DataType::Utf8).unwrap();
        let kind = kind.as_string::<i32>();
        let ts = |n: &str| -> Vec<Option<i64>> {
            let c = arrow::compute::cast(b.column_by_name(n).unwrap(), &arrow::datatypes::DataType::Timestamp(arrow::datatypes::TimeUnit::Nanosecond, None)).unwrap();
            let a = c.as_primitive::<TimestampNanosecondType>().clone();
            (0..a.len()).map(|i| (!a.is_null(i)).then(|| a.value(i))).collect()
        };
        let (tt, rr, label, at) = (ts("t"), ts("r"), ts("label"), ts("at"));
        for i in 0..b.num_rows() {
            let k = kind.value(i).to_string();
            let (x, y) = if k == "barrier" {
                (label[i].unwrap(), at[i].unwrap())
            } else {
                (tt[i].unwrap_or(0), rr[i].unwrap_or(0))
            };
            out.push((seq.value(i), k, x, y));
        }
    }
    out
}

fn live_spec(svc: &DataService, clock: VersionRef) -> StreamSpec {
    let st = svc.store();
    let mut relations: Vec<VersionRef> = ["equity.close", "equity.volume"]
        .iter()
        .map(|r| VersionRef {
            relation: r.to_string(),
            version: st.registry.current(r, Some("s1")).unwrap().to_string(),
        })
        .collect();
    relations.push(clock);
    StreamSpec {
        relations: Some(relations),
        binding: None,
        calendars: Some(vec!["base".into()]),
        receipt_filter: None,
        clock: ClockSource::Live,
        session: Some("s1".into()),
    }
}

#[test]
fn a_live_stream_emits_its_barriers_after_the_bars_accepted_within_grace_and_replays() {
    let (svc, labels, recorder) = session("live-stream");
    let c = Client::connect(inproc(&svc), Layer::Kernel, "test").unwrap();
    let plan = planned(&svc, "r1", &labels);
    let spec = live_spec(&svc, plan);
    let live = rows(&events(&c, spec, false));
    let state = recorder.join().unwrap().unwrap();
    assert_eq!((state.batches, state.rows), (6, 60), "{:?}", state);
    // Six barriers, each emitted at its label plus the grace or later, and
    // after both names' bars of its minute (accepted 50 ms after the close).
    let barriers: Vec<&(u64, String, i64, i64)> = live.iter().filter(|e| e.1 == "barrier").collect();
    assert_eq!(barriers.len(), 6);
    for (b, &l) in barriers.iter().zip(&labels) {
        assert_eq!(b.2, l);
        assert!(b.3 >= l + GRACE, "barrier {} emitted at {}", l, b.3);
    }
    for (k, b) in barriers.iter().enumerate() {
        let before = live.iter().filter(|e| e.1 == "tuple" && e.0 < b.0 && e.2 == labels[k]).count();
        assert_eq!(before, 4, "the bars of {} precede its barrier", labels[k]);
    }
    // Every row received at its acceptance: after its close plus the delay,
    // and in order.
    let tuples: Vec<&(u64, String, i64, i64)> = live.iter().filter(|e| e.1 == "tuple").collect();
    assert!(tuples.iter().all(|e| e.3 >= e.2 + 50_000_000));
    assert!(tuples.windows(2).all(|w| w[0].3 <= w[1].3));
    // seq is gap-free from 1.
    assert!(live.iter().enumerate().all(|(i, e)| e.0 == i as u64 + 1));
    // The recorded stream: the session's versions and the recorded clock.
    let recorded = {
        let st = svc.store();
        let clock = st.registry.current("kernel.r1.clock", Some("live")).unwrap().to_string();
        assert_eq!(st.read_all(&clock).unwrap().num_rows(), 6);
        let mut relations: Vec<VersionRef> = ["equity.close", "equity.volume"]
            .iter()
            .map(|r| VersionRef {
                relation: r.to_string(),
                version: st.registry.current(r, Some("s1")).unwrap().to_string(),
            })
            .collect();
        relations.push(VersionRef {
            relation: "kernel.r1.clock".into(),
            version: clock,
        });
        StreamSpec {
            relations: Some(relations),
            binding: None,
            calendars: Some(vec!["base".into()]),
            receipt_filter: None,
            clock: ClockSource::Replay,
            session: None,
        }
    };
    // Proposition D-5.4: the replay is the live stream, seq for seq.
    let replay = rows(&events(&c, recorded.clone(), false));
    assert_eq!(replay, live);
    // A paced replay takes the session's span on the clock: five minutes
    // between the first barrier and the last, 2.5 s of wall time at 120x.
    let start = Instant::now();
    let paced = rows(&events(&c, recorded, true));
    assert!(start.elapsed() >= Duration::from_millis(2400), "{:?}", start.elapsed());
    assert_eq!(paced, live);
}

#[test]
fn a_live_stream_of_a_session_that_does_not_exist_is_refused() {
    let (svc, labels, recorder) = session("live-none");
    let c = Client::connect(inproc(&svc), Layer::Kernel, "test").unwrap();
    let plan = planned(&svc, "r2", &labels);
    let mut spec = live_spec(&svc, plan);
    spec.session = Some("nope".into());
    let mut reply = c
        .call(Message::SubscribeRequest(Box::new(SubscribeRequest {
            spec: Box::new(spec),
            from_seq: 0,
            paced: false,
            to_seq: None,
            with_index: false,
        })))
        .unwrap();
    let err = reply.message().err().unwrap().to_string();
    assert!(err.contains("no live session `nope`"), "{}", err);
    recorder.join().unwrap().unwrap();
}
