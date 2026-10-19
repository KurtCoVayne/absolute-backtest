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
    session_named(tag, "g4", bars)
}

/// `session` with the session's name given.
fn session_named(tag: &str, name: &str, bars: i64) -> (Arc<DataService>, Arc<dyn Clock>, abt_data::live::Recording) {
    session_with(tag, name, bars, None)
}

/// `session_named` over a store that, with `decoy`, already holds the
/// same instruments from another feed (other prices, no bar) under that
/// source name.
fn session_with(tag: &str, name: &str, bars: i64, decoy: Option<&str>) -> (Arc<DataService>, Arc<dyn Clock>, abt_data::live::Recording) {
    let labels: Vec<i64> = (1..=bars).map(|k| abt_wire::scalar::parse_timestamp("2024-03-11T14:30:00").unwrap() + k * MIN).collect();
    let clock: Arc<dyn Clock> = Arc::new(ScaledClock::new(labels[0] - MIN / 2, RATE));
    let grace: BTreeMap<String, i64> = [("base".to_string(), 20_000_000_000)].into_iter().collect();
    let feed = SyntheticFeed::new(clock.clone(), &["AAA", "BBB"], labels.clone(), 50_000_000, 11);
    let mut store = Store::temporary(tag).unwrap();
    if let Some(source) = decoy {
        // The day before, so that no row of the decoy is one of the session's.
        let earlier: Vec<i64> = labels.iter().map(|l| l - 24 * 60 * MIN).collect();
        let other = SyntheticFeed::new(clock.clone(), &["AAA", "BBB"], earlier, 50_000_000, 12);
        other.prelude().unwrap().ingest(&mut store, "synthetic", source, &BTreeMap::new()).unwrap();
    }
    let svc = DataService::with_clock(store, Sources::store_only(), clock.clone(), grace);
    svc.open_session(name, "synthetic", &feed.prelude().unwrap(), &labels).unwrap();
    let recorder = svc.record(name, "synthetic", Box::new(feed));
    (svc, clock, recorder)
}

fn live(prog: &abt_kernel::check::Program, svc: &Arc<DataService>, clock: &Arc<dyn Clock>, deadline: Option<i64>) -> RunResult {
    live_in(prog, svc, clock, deadline, "g4")
}

/// `live` over the session named.
fn live_in(prog: &abt_kernel::check::Program, svc: &Arc<DataService>, clock: &Arc<dyn Clock>, deadline: Option<i64>, session: &str) -> RunResult {
    let client = DataClient::connect(inproc(svc)).unwrap();
    let c = clock.clone();
    let opts = Replay {
        mode: RunMode::LiveSim { session: session.into() },
        clock: Some(LiveClock(Arc::new(move || c.now()))),
        deadline,
        write_outputs: true,
        ..Replay::default()
    };
    run_on(prog, None, &client, ExecConfig::from_program(prog), &opts).unwrap()
}

/// A-D55: a run's id says which live session it read or which recording
/// it replays. Two sessions of one program over alike stores are two runs
/// with their outputs under their own ids, and the replays of their
/// recordings two runs again; through `bind::plan_step`, a run over the
/// whole stream keeps an empty π (its id is unchanged), the same session
/// gives the same π, and two sessions or two recordings differ.
#[test]
fn a_run_id_names_the_session_it_reads_or_the_recording_it_replays() {
    use abt_kernel::kernel::bind::{plan_step, run_id};
    let prog = common::program(&source("late send"), "g4");
    let mut runs = Vec::new();
    for (tag, name) in [("g4-id-a", "s1"), ("g4-id-b", "s2")] {
        let (svc, clock, recorder) = session_named(tag, name, 4);
        let run = live_in(&prog, &svc, &clock, None, name);
        recorder.join().unwrap().unwrap();
        runs.push((svc, run));
    }
    let (a, b) = (&runs[0].1, &runs[1].1);
    assert_eq!(a.bars.len(), 4);
    assert_ne!(a.run_id, b.run_id, "two sessions, two runs");
    let replay = |svc: &Arc<DataService>, of: &str| -> String {
        let client = DataClient::connect(inproc(svc)).unwrap();
        let opts = Replay {
            replay_of: Some(of.to_string()),
            write_outputs: true,
            ..Replay::default()
        };
        run_on(&prog, None, &client, ExecConfig::from_program(&prog), &opts).unwrap().run_id
    };
    let (ra, rb) = (replay(&runs[0].0, &a.run_id), replay(&runs[1].0, &b.run_id));
    assert_ne!(ra, rb, "two recordings, two replays");
    assert_ne!(ra, a.run_id, "two modes, two runs");
    // The identity function itself, over one program and binding.
    let cfg = ExecConfig::from_program(&prog);
    let step = |session: Option<&str>, of: Option<&str>| plan_step(None, &cfg, None, None, session, of);
    let id = |session: Option<&str>, of: Option<&str>, mode: &str| run_id("H", "B", &step(session, of), mode);
    assert_eq!(step(None, None), serde_json::Value::Null, "a run over the whole stream has no π: its id is as before");
    assert_eq!(id(Some("s1"), None, "live-sim"), id(Some("s1"), None, "live-sim"));
    assert_ne!(id(Some("s1"), None, "live-sim"), id(Some("s2"), None, "live-sim"));
    assert_ne!(id(Some("s1"), None, "live-sim"), id(Some("s1"), None, "live-broker"));
    assert_ne!(id(None, Some(&a.run_id), "replay-sim"), id(None, Some(&b.run_id), "replay-sim"));
    assert_ne!(id(None, Some(&a.run_id), "replay-sim"), id(None, None, "replay-sim"));
}

/// A-D56: a live run binds its session's versions and its replay the
/// same, whatever other sources the store holds: a decoy version of the
/// session's relations from a source that sorts first (`a0`: the same
/// instruments from another feed, no bar) is in the store before the
/// session opens.
#[test]
fn a_live_run_and_its_replay_bind_the_session_among_other_sources() {
    let prog = common::program(&source("late send"), "g4");
    let (svc, clock, recorder) = session_with("g4-decoy", "s2", 6, Some("a0"));
    let live_run = live_in(&prog, &svc, &clock, None, "s2");
    recorder.join().unwrap().unwrap();
    assert!(!live_run.fills.is_empty(), "the session trades: {:?}", events(&live_run));
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
    assert_eq!(events(&live_run), events(&replayed));
    assert_eq!(live_run.bars, replayed.bars);
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
