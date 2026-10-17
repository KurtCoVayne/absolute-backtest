//! G4 (Proposition A-4.4, K-6.2, D-5.4, D-5.5): a `live-sim` run over a
//! session the data layer records from a synthetic feed, and a
//! `replay-sim` run of that run's recorded stream, produce identical
//! decision and execution events. The clock runs 240 times the wall clock
//! and the bars' grace is twenty seconds (83 ms of wall time), so every bar
//! is received before its barrier; a deadline of zero makes every bar late,
//! and `late reject` drops what it decides.

use abt_tests as common;

use std::collections::BTreeMap;
use std::sync::Arc;

use abt_data::live::{Clock, ScaledClock, SyntheticFeed};
use abt_data::resolver::Sources;
use abt_data::service::{inproc, DataService};
use abt_data::Store;
use abt_kernel::kernel::{run_on, DataClient, ExecConfig, LiveClock, Replay, RunMode, RunResult};

const MIN: i64 = 60_000_000_000;
const RATE: f64 = 240.0;

/// A minute strategy over the bars alone: buy a name that closed up while
/// flat, sell what is held when it closes down.
fn source(late: &str) -> String {
    format!(
        "strategy g4 {{
  data {{ class equity   need bars @1m   allow unreviewed   prefer latest }}
  uses calendar, bars, book
  clock {{ decide on base }}
  universe {{ universe(A, T) :- traded@base(A, T). }}
  params {{
    qty : Quantity<Shares> = 10 shares
  }}
  rules {{
    rel up(A: Equity, @T)
    up(A, T) :- universe(A, T), close(A, T, P), prev(T, T0), close(A, T0, P0), P > P0.
    rel down(A: Equity, @T)
    down(A, T) :- universe(A, T), close(A, T, P), prev(T, T0), close(A, T0, P0), P <= P0.
  }}
  decisions {{
    silence hold
    orders market
    decide(T, trade(A, qty)) :- up(A, T), flat(A, T).
    decide(T, trade(A, Q)) :- down(A, T), held(A, T, Q0), Q = -Q0.
  }}
  execution equity_default with {{ {} }}
  report {{ calendar base   periods 98280 per year }}
}}
",
        late
    )
}

/// What a run decided and executed, in order: every event's kind, time,
/// instrument and what it carries, exactly.
fn events(r: &RunResult) -> Vec<String> {
    r.events
        .iter()
        .map(|e| format!("{} {} {:?} {:?} {:?}", e.kind.name(), e.t, e.instrument.map(|s| r.symbols[s as usize].clone()), e.order_id, e.kind))
        .collect()
}

/// A session of `bars` one-minute bars of two names, recording on a clock
/// that starts half a minute before the first.
fn session(tag: &str, bars: i64) -> (Arc<DataService>, Arc<dyn Clock>, abt_data::live::Recording) {
    let labels: Vec<i64> = (1..=bars).map(|k| abt_wire::scalar::parse_timestamp("2024-03-11T14:30:00").unwrap() + k * MIN).collect();
    let clock: Arc<dyn Clock> = Arc::new(ScaledClock::new(labels[0] - MIN / 2, RATE));
    let grace: BTreeMap<String, i64> = [("base".to_string(), 20_000_000_000)].into_iter().collect();
    let svc = DataService::with_clock(Store::temporary(tag).unwrap(), Sources::store_only(), clock.clone(), grace);
    let feed = SyntheticFeed::new(clock.clone(), &["AAA", "BBB"], labels.clone(), 50_000_000, 11);
    svc.open_session("g4", "synthetic", &feed.prelude().unwrap(), &labels).unwrap();
    let recorder = svc.record("g4", "synthetic", Box::new(feed));
    (svc, clock, recorder)
}

fn live(prog: &abt_kernel::check::Program, svc: &Arc<DataService>, clock: &Arc<dyn Clock>, deadline: Option<i64>) -> RunResult {
    let client = DataClient::connect(inproc(svc)).unwrap();
    let c = clock.clone();
    let opts = Replay {
        mode: RunMode::LiveSim { session: "g4".into() },
        clock: Some(LiveClock(Arc::new(move || c.now()))),
        deadline,
        write_outputs: true,
        ..Replay::default()
    };
    run_on(prog, None, &client, ExecConfig::from_program(prog), &opts).unwrap()
}

#[test]
fn replay_of_a_live_session_equals_the_live_run() {
    let prog = common::program(&source("late send"), "g4");
    let (svc, clock, recorder) = session("g4", 12);
    let live_run = live(&prog, &svc, &clock, None);
    let state = recorder.join().unwrap().unwrap();
    assert_eq!(state.batches, 12, "{:?}", state);
    assert!(!live_run.fills.is_empty(), "the session trades: {:?}", events(&live_run));
    assert!(live_run.late_bars.is_empty(), "no bar is late within a bar's deadline");
    // The replay of the recorded stream, at full speed.
    let client = DataClient::connect(inproc(&svc)).unwrap();
    let replayed = run_on(
        &prog,
        None,
        &client,
        ExecConfig::from_program(&prog),
        &Replay {
            replay_of: Some(live_run.run_id.clone()),
            write_outputs: true,
            ..Replay::default()
        },
    )
    .unwrap();
    assert_ne!(live_run.run_id, replayed.run_id, "two modes, two runs");
    assert_eq!(events(&live_run), events(&replayed));
    assert_eq!(live_run.bars, replayed.bars);
    // Every bar's close was received within the grace, so every decision
    // bar saw its own close: a bar's decisions read the bar.
    assert_eq!(live_run.bars.len(), 12);
}

#[test]
fn a_bar_evaluated_past_its_deadline_is_late_and_reject_drops_its_decisions() {
    let prog = common::program(&source("late reject"), "g4");
    let (svc, clock, recorder) = session("g4-late", 6);
    // A deadline of nothing: every bar is evaluated after its barrier.
    let run = live(&prog, &svc, &clock, Some(-1));
    recorder.join().unwrap().unwrap();
    assert_eq!(run.late_bars.len(), run.bars.len());
    assert!(run.fills.is_empty(), "a rejected decision is never sent");
    assert!(run.dropped.iter().all(|(_, _, why)| why == "late"), "{:?}", run.dropped);
    assert!(!run.dropped.is_empty(), "the strategy decided something");
    assert!(run.warnings.iter().any(|w| w.bias == "latency" && w.message.contains("dropped")), "{:?}", run.warnings);
}
