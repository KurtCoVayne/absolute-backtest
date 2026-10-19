//! The live interface's gate (M-10; K-6.4, K-6.5): `BrokerExecutor` over
//! the mock broker. Proposition A-4.3: on the same stream (a store's replay,
//! a recorded live session), a `live-broker` run decides what the
//! simulation decides, and its fills are the broker's, as the mock
//! dictates. Then the order lifecycle (new, partial, filled, cancelled,
//! rejected, expired; one working order per instrument, replaced by a new
//! decision), reconciliation under `halt`, `adopt` and `warn` at a
//! session's open and on a reconnect, the kill switch (`halt_all` by a
//! control request, a file; `flatten_all` through decisions), and the
//! deployment document, its hash and the refusals.

#![allow(clippy::result_large_err)]

use abt_tests as common;

use std::collections::BTreeMap;
use std::sync::Arc;

use abt_data::live::{Clock, ScaledClock, SyntheticFeed};
use abt_data::resolver::Sources;
use abt_data::service::{inproc, DataService};
use abt_data::Store;
use abt_kernel::check::{Program, NO_RULE};
use abt_kernel::contract::Status;
use abt_kernel::kernel::events::{Book, Fate};
use abt_kernel::kernel::{
    executor_for, on_kernel_thread, replay, replay_observed, run_on, Ctor, DataClient, Dataset, EventKind, ExecConfig, LiveClock, Next, Progress, Replay, RunError, RunMode, RunResult, SimExecutor,
};
use abt_kernel::live::lifecycle::{Lifecycle, LiveOrder};
use abt_kernel::live::{self, deployment, BrokerExecutor, Deployment, HaltSource, LiveControl, MockBroker, OrderState, Policy, Report, Scripted};
use abt_tests::{StrategyParts, FRICTIONLESS};
use abt_wire::generated::abt::wire::{QueryForm, Scan, View};
use arrow::array::{Array, AsArray, RecordBatch};

// ---------------------------------------------------------------- helpers

/// A deployment of the mock broker with `fields` added.
fn deployment(fields: &str) -> Deployment {
    deployment::parse(&format!("deployment rehearsal {{\n  account DU0001   broker mock\n  {}\n}}", fields)).unwrap_or_else(|e| panic!("{}", e))
}

/// A `live-broker` run over `mock`: a rehearsal over the data's replay, or
/// over the recorded run `opts` names.
fn broker_run(prog: &Program, client: &DataClient, cfg: ExecConfig, mock: &MockBroker, d: &Deployment, opts: Replay) -> Result<RunResult, RunError> {
    let (mock, d) = (mock.clone(), d.clone());
    on_kernel_thread(move || {
        let mut exec = BrokerExecutor::new(cfg.clone(), Box::new(mock), d.clone());
        let opts = Replay {
            mode: RunMode::LiveBroker { session: None },
            deployment: Some(d),
            ..opts
        };
        replay(prog, None, client, cfg, &mut exec, &opts, &mut |_| {}).map(|(r, k)| {
            std::mem::forget(k);
            r
        })
    })
}

/// `broker_run`, with the run's id even when it halts (its outputs are in
/// the store).
fn broker_run_named(prog: &Program, client: &DataClient, cfg: ExecConfig, mock: &MockBroker, d: &Deployment, opts: Replay) -> (Result<RunResult, RunError>, String) {
    let (mock, d) = (mock.clone(), d.clone());
    let mut id = String::new();
    let out = on_kernel_thread(|| {
        let mut exec = BrokerExecutor::new(cfg.clone(), Box::new(mock), d.clone());
        let opts = Replay {
            mode: RunMode::LiveBroker { session: None },
            deployment: Some(d),
            write_outputs: true,
            ..opts
        };
        replay_observed(prog, None, client, cfg, &mut exec, &opts, &mut |_| {}, &mut |p, _, _| {
            if let Progress::Started { run_id } = p {
                id = run_id;
            }
            Ok(Next::Continue)
        })
        .map(|(r, k)| {
            std::mem::forget(k);
            r
        })
    });
    (out, id)
}

/// The same stream through the simulation.
fn sim_run(prog: &Program, client: &DataClient, cfg: ExecConfig, opts: Replay) -> RunResult {
    on_kernel_thread(move || {
        let mut exec = SimExecutor::new(cfg.clone());
        replay(prog, None, client, cfg, &mut exec, &opts, &mut |_| {}).map(|(r, k)| {
            std::mem::forget(k);
            r
        })
    })
    .unwrap()
}

/// The run's decision events in order: bar, instrument, rule and the
/// decision.
fn decisions(r: &RunResult) -> Vec<String> {
    r.events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::Decision { decision } => Some(format!("{} {} {:?} {}", e.t, r.symbols[decision.equity as usize], e.rule, r.describe_decision(decision))),
            _ => None,
        })
        .collect()
}

fn fills(r: &RunResult) -> Vec<String> {
    r.fills
        .iter()
        .map(|f| format!("{} {} {} @ {} ({})", f.t, r.symbols[f.equity as usize], f.quantity, f.price, f.commission))
        .collect()
}

/// The events of `kind` (its K-5.2 name).
fn of_kind<'r>(r: &'r RunResult, kind: &str) -> Vec<&'r abt_kernel::kernel::events::RunEvent> {
    r.events.iter().filter(|e| e.kind.name() == kind).collect()
}

fn status<'r>(r: &'r RunResult, clause: &str) -> &'r Status {
    &r.contract.clauses.iter().find(|c| c.id == clause).unwrap_or_else(|| panic!("no clause {}", clause)).status
}

/// Every decision is an order with one end, or dropped before it was one
/// (K-3.4 `post.decisions`).
fn accounted(r: &RunResult) {
    assert!(matches!(status(r, "post.decisions"), Status::Checked { .. }), "{:?}", status(r, "post.decisions"));
}

fn sym(r: &RunResult, name: &str) -> u32 {
    r.symbols.iter().position(|s| s == name).unwrap_or_else(|| panic!("no {}", name)) as u32
}

/// Two names over ten weekdays from 2024-01-08.
fn two_names() -> Dataset {
    common::markets(
        (2024, 1, 8),
        &[
            ("AAA", &[10.0, 10.5, 11.0, 10.8, 11.2, 11.5, 11.1, 11.8, 12.0, 12.2]),
            ("BBB", &[20.0, 19.5, 19.8, 20.4, 20.1, 20.6, 21.0, 20.7, 21.3, 21.5]),
        ],
    )
}

/// Half the fixed base in every name at every bar, frictionless.
fn halves() -> Program {
    let src = StrategyParts {
        name: "halves".into(),
        params: "    w : Scalar = 0.5".into(),
        decides: "    decide(T, hold(A, w)) :- universe(A, T).".into(),
        execution: FRICTIONLESS.into(),
        ..Default::default()
    }
    .render();
    common::program(&src, "halves")
}

/// A hundred shares of every name at the first bar, nothing after.
fn buy_once() -> Program {
    let src = StrategyParts {
        name: "buy_once".into(),
        params: "    qty : Quantity<Shares> = 100 shares".into(),
        rules: "    rel later(@T)\n    later(T) :- bar(T), prev(T, _).".into(),
        decides: "    decide(T, trade(A, qty)) :- universe(A, T), not later(T).".into(),
        execution: FRICTIONLESS.into(),
        ..Default::default()
    }
    .render();
    common::program(&src, "buy_once")
}

/// A relation of the store, every row.
fn scan(client: &DataClient, relation: &str) -> RecordBatch {
    let view = View {
        versions: None,
        binding: None,
        as_of_seq: None,
    };
    let form = QueryForm::Scan(Box::new(Scan {
        relation: relation.to_string(),
        t0: i64::MIN,
        t1: i64::MAX,
        predicate: None,
        columns: None,
    }));
    let batches = client.query(&view, form).unwrap_or_else(|e| panic!("{}: {}", relation, e));
    let schema = batches.first().map(|b| b.schema()).unwrap_or_else(|| panic!("{} has no rows", relation));
    arrow::compute::concat_batches(&schema, &batches).unwrap()
}

/// A label or text column's values.
fn texts(b: &RecordBatch, col: &str) -> Vec<Option<String>> {
    let c = b.column_by_name(col).unwrap_or_else(|| panic!("no column {}", col));
    let c = arrow::compute::cast(c, &arrow::datatypes::DataType::Utf8).unwrap();
    let s = c.as_string::<i32>();
    (0..s.len()).map(|i| (!s.is_null(i)).then(|| s.value(i).to_string())).collect()
}

// ------------------------------------------------- Proposition A-4.3

/// The corpus programs whose decisions read no book relation, and one that
/// reads it as `held` and `flat` with orders of a fixed size.
const BOOK_FREE: [&str; 6] = ["low_volatility", "momentum_12_1", "short_term_reversal", "size_proxy", "volatility_targeting", "sma_crossover"];

#[test]
fn on_a_stores_replay_the_broker_run_decides_as_the_simulation() {
    for name in BOOK_FREE {
        let file = common::corpus_dir().join("strategies").join(format!("{}.dsl", name));
        let (prog, ds) = common::golden_inputs(&file).unwrap();
        let cfg = ExecConfig::from_program(&prog);
        let client = common::client(&prog, &ds);
        let sim = sim_run(&prog, &client, cfg.clone(), Replay::default());
        // The mock's account does not carry the data's corporate actions:
        // the book and the account apart is reported, not halted on.
        let mock = MockBroker::new(cfg.initial_cash);
        let live = broker_run(&prog, &client, cfg.clone(), &mock, &deployment("reconcile warn"), Replay::default()).unwrap_or_else(|e| panic!("{}: {}", name, e));
        assert_ne!(sim.run_id, live.run_id, "{}: two modes, two runs", name);
        let (a, b) = (decisions(&sim), decisions(&live));
        assert!(!a.is_empty(), "{} decides", name);
        if let Some(i) = (0..a.len().max(b.len())).find(|&i| a.get(i) != b.get(i)) {
            panic!("{}: the decisions part at {}: {:?} in the simulation, {:?} through the broker", name, i, a.get(i), b.get(i));
        }
        // The fills are the broker's: at the price each order was sized at,
        // without commission, where the simulation fills at the next bar
        // with its costs.
        assert_ne!(fills(&sim), fills(&live), "{}", name);
        assert!(
            live.fills.iter().all(|f| f.forced || f.broker.is_some()),
            "{}: every fill the program's orders made came from the broker",
            name
        );
        assert!(live.fills.iter().filter(|f| f.broker.is_some()).all(|f| f.commission == 0.0), "{}", name);
        assert!(!mock.sent().is_empty() && mock.sent().iter().all(|(_, o)| o.reference.is_some()), "{}", name);
        accounted(&live);
    }
}

/// A minute strategy over the bars alone (G4's): buy a name that closed up
/// while flat, sell what is held when it closes down.
const G4: &str = "strategy g4 {
  data { class equity   need bars @1m   allow unreviewed   prefer latest }
  uses calendar, bars, book
  clock { decide on base }
  universe { universe(A, T) :- traded@base(A, T). }
  params {
    qty : Quantity<Shares> = 10 shares
  }
  rules {
    rel up(A: Equity, @T)
    up(A, T) :- universe(A, T), close(A, T, P), prev(T, T0), close(A, T0, P0), P > P0.
    rel down(A: Equity, @T)
    down(A, T) :- universe(A, T), close(A, T, P), prev(T, T0), close(A, T0, P0), P <= P0.
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- up(A, T), flat(A, T).
    decide(T, trade(A, Q)) :- down(A, T), held(A, T, Q0), Q = -Q0.
  }
  execution equity_default with { late send }
  report { calendar base   periods 98280 per year }
}
";

const MIN: i64 = 60_000_000_000;

#[test]
fn a_rehearsal_of_a_recorded_live_session_decides_as_the_live_sim_run() {
    let prog = common::program(G4, "g4");
    // A session of twelve one-minute bars of two names, recorded from a
    // synthetic feed on a clock 240 times the wall's (G4).
    let labels: Vec<i64> = (1..=12).map(|k| abt_wire::scalar::parse_timestamp("2024-03-11T14:30:00").unwrap() + k * MIN).collect();
    let clock: Arc<dyn Clock> = Arc::new(ScaledClock::new(labels[0] - MIN / 2, 240.0));
    let grace: BTreeMap<String, i64> = [("base".to_string(), 20_000_000_000)].into_iter().collect();
    let svc = DataService::with_clock(Store::temporary("m10-live").unwrap(), Sources::store_only(), clock.clone(), grace);
    let feed = SyntheticFeed::new(clock.clone(), &["AAA", "BBB"], labels.clone(), 50_000_000, 11);
    svc.open_session("m10", "synthetic", &feed.prelude().unwrap(), &labels).unwrap();
    let recorder = svc.record("m10", "synthetic", Box::new(feed));
    let c = clock.clone();
    let live_sim = run_on(
        &prog,
        None,
        &DataClient::connect(inproc(&svc)).unwrap(),
        ExecConfig::from_program(&prog),
        &Replay {
            mode: RunMode::LiveSim { session: "m10".into() },
            clock: Some(LiveClock(Arc::new(move || c.now()))),
            write_outputs: true,
            ..Replay::default()
        },
    )
    .unwrap();
    recorder.join().unwrap().unwrap();
    assert!(!live_sim.fills.is_empty(), "the session trades");
    // The broker's rehearsal of the recorded stream.
    let client = DataClient::connect(inproc(&svc)).unwrap();
    let cfg = ExecConfig::from_program(&prog);
    let mock = MockBroker::new(cfg.initial_cash);
    let rehearsal = broker_run(
        &prog,
        &client,
        cfg,
        &mock,
        &deployment(""),
        Replay {
            replay_of: Some(live_sim.run_id.clone()),
            write_outputs: true,
            ..Replay::default()
        },
    )
    .unwrap();
    assert_eq!(rehearsal.bars, live_sim.bars);
    assert_eq!(decisions(&live_sim), decisions(&rehearsal));
    assert_ne!(fills(&live_sim), fills(&rehearsal));
    assert!(rehearsal.fills.iter().all(|f| f.broker.is_some()));
    accounted(&rehearsal);
}

/// The mock fills half an order at once and the rest at its next report:
/// a decision on the instrument meanwhile replaces what works, and the
/// decisions are still the simulation's.
#[test]
fn fills_are_as_the_mock_dictates_and_decisions_do_not_follow_them() {
    let (prog, ds) = common::golden_inputs(&common::corpus_dir().join("strategies/momentum_12_1.dsl")).unwrap();
    let cfg = ExecConfig::from_program(&prog);
    let client = common::client(&prog, &ds);
    let sim = sim_run(&prog, &client, cfg.clone(), Replay::default());
    let mock = MockBroker::with_policy(cfg.initial_cash, |o, asked| {
        let p = o.reference.unwrap_or(0.0);
        let half = (o.quantity / 2.0).trunc();
        match asked {
            0 if half != 0.0 => vec![
                Report::New,
                Report::Partial {
                    quantity: half,
                    price: p,
                    commission: 1.0,
                },
            ],
            0 => vec![
                Report::New,
                Report::Filled {
                    quantity: o.quantity,
                    price: p,
                    commission: 1.0,
                },
            ],
            _ => vec![Report::Filled {
                quantity: o.quantity - half,
                price: p,
                commission: 1.0,
            }],
        }
    });
    let live = broker_run(&prog, &client, cfg, &mock, &deployment("reconcile warn"), Replay::default()).unwrap();
    assert_eq!(decisions(&sim), decisions(&live));
    let partial: Vec<_> = live
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::Fill { remainder: Some((left, fate)), .. } => Some((*left, *fate)),
            _ => None,
        })
        .collect();
    assert!(!partial.is_empty() && partial.iter().all(|(left, fate)| *left > 0.0 && *fate == Fate::Working), "{:?}", partial);
    assert!(live.fills.iter().filter(|f| f.broker.is_some()).all(|f| f.commission == 1.0));
    accounted(&live);
}

// ------------------------------------------------------ the lifecycle

fn order(id: &str, sym: u32, quantity: f64) -> LiveOrder {
    LiveOrder {
        broker_id: id.into(),
        order_id: 0,
        instrument: sym,
        decision: abt_kernel::kernel::Decision::single(Ctor::Trade, sym, quantity, Default::default()),
        rule: 0,
        decided: 1,
        quantity,
        filled: 0.0,
        state: OrderState::Sent,
        history: Vec::new(),
    }
}

#[test]
fn the_lifecycle_admits_its_transitions_and_no_other() {
    use OrderState::*;
    let fill = |q: f64| Report::Filled {
        quantity: q,
        price: 1.0,
        commission: 0.0,
    };
    let part = |q: f64| Report::Partial {
        quantity: q,
        price: 1.0,
        commission: 0.0,
    };
    let why = || "a reason".to_string();
    let admitted = [
        (Sent, Report::New, New),
        (Sent, part(1.0), Partial),
        (New, part(1.0), Partial),
        (Partial, part(1.0), Partial),
        (Sent, fill(1.0), Filled),
        (New, fill(1.0), Filled),
        (Partial, fill(1.0), Filled),
        (New, Report::Cancelled { reason: why() }, Cancelled),
        (Partial, Report::Cancelled { reason: why() }, Cancelled),
        (Sent, Report::Rejected { reason: why() }, Rejected),
        (New, Report::Rejected { reason: why() }, Rejected),
        (New, Report::Expired { reason: why() }, Expired),
        (Partial, Report::Expired { reason: why() }, Expired),
        // A superseded order's last reports: its cancel, or a fill that
        // crossed it.
        (Replaced, Report::Cancelled { reason: why() }, Replaced),
        (Replaced, fill(1.0), Replaced),
    ];
    for (from, report, to) in admitted {
        assert_eq!(from.after(&report), Ok(to), "{:?} on {:?}", report, from);
    }
    let refused = [
        (New, Report::New),
        (Partial, Report::Rejected { reason: why() }),
        (Filled, fill(1.0)),
        (Filled, part(1.0)),
        (Cancelled, fill(1.0)),
        (Rejected, Report::New),
        (Expired, fill(1.0)),
        (Replaced, Report::Rejected { reason: why() }),
    ];
    for (from, report) in refused {
        assert!(from.after(&report).is_err(), "{:?} on {:?} is admitted", report, from);
    }
    // One order works per instrument; an end frees it.
    let mut l = Lifecycle::default();
    l.sent(order("M1", 3, 10.0)).unwrap();
    assert!(l.sent(order("M2", 3, 5.0)).is_err(), "a second order on the instrument");
    l.sent(order("M3", 4, 5.0)).unwrap();
    assert_eq!(l.working(), vec![3, 4]);
    l.report("M1", 2, &Report::New).unwrap();
    assert_eq!(l.report("M1", 2, &part(4.0)).unwrap().remaining(), 6.0);
    assert_eq!(l.report("M1", 3, &fill(6.0)).unwrap().state, Filled);
    assert_eq!(l.working(), vec![4]);
    assert_eq!(l.get("M1").unwrap().history, vec![(1, Sent), (2, New), (2, Partial), (3, Filled)]);
    assert!(l.report("M1", 4, &fill(1.0)).is_err(), "a filled order reports no more fills");
    assert!(l.report("M9", 4, &Report::New).is_err(), "an order the run did not send");
    l.superseded("M3", 5);
    assert!(l.working().is_empty());
    assert_eq!(l.report("M3", 5, &Report::Cancelled { reason: "replaced".into() }).unwrap().state, Replaced);
    l.sent(order("M4", 3, 1.0)).unwrap();
}

/// The broker's reports, through a run: an order acknowledged, part filled
/// and filled at its next report; one rejected; one expired; one cancelled
/// by the broker.
#[test]
fn an_order_ends_as_the_broker_reports_it() {
    let prog = buy_once();
    let ds = two_names();
    let cfg = ExecConfig::from_program(&prog);
    let client = common::client(&prog, &ds);
    let run = |policy: fn(&live::OrderRequest, u32) -> Vec<Report>| {
        let mock = MockBroker::with_policy(cfg.initial_cash, policy);
        let r = broker_run(&prog, &client, cfg.clone(), &mock, &deployment("reconcile warn"), Replay::default()).unwrap();
        accounted(&r);
        r
    };
    // New, then sixty of the hundred, then the forty.
    let r = run(|o, asked| match asked {
        0 => vec![
            Report::New,
            Report::Partial {
                quantity: 60.0,
                price: o.reference.unwrap(),
                commission: 0.5,
            },
        ],
        _ => vec![Report::Filled {
            quantity: 40.0,
            price: o.reference.unwrap() + 0.25,
            commission: 0.5,
        }],
    });
    let kinds: Vec<&str> = r.events.iter().filter(|e| e.instrument == Some(sym(&r, "AAA"))).map(|e| e.kind.name()).collect();
    assert_eq!(kinds, vec!["decision", "order_sent", "partial_fill", "fill"], "{:?}", kinds);
    let (b1, b2, b3) = (r.bars[0], r.bars[1], r.bars[2]);
    let aaa: Vec<_> = r
        .fills
        .iter()
        .filter(|f| r.symbols[f.equity as usize] == "AAA")
        .map(|f| (f.t, f.quantity, f.price, f.partial))
        .collect();
    assert_eq!(aaa, vec![(b2, 60.0, 10.0, true), (b3, 40.0, 10.25, false)]);
    let partial = of_kind(&r, "partial_fill");
    assert!(partial.iter().all(|e| matches!(e.kind, EventKind::Fill { remainder: Some((left, Fate::Working)), .. } if left == 40.0)));
    // The book holds the hundred, at the broker's prices and commissions.
    assert_eq!(r.final_positions.get(&sym(&r, "AAA")).copied(), Some(100.0));
    assert!(
        r.events.iter().filter(|e| e.t == b1).all(|e| !matches!(e.kind, EventKind::Fill { .. })),
        "nothing fills at the bar it was sent"
    );
    // Refused, with the broker's reason.
    let r = run(|_, _| vec![Report::Rejected { reason: "no buying power".into() }]);
    let rejected = of_kind(&r, "rejected");
    assert_eq!(rejected.len(), 2);
    assert!(rejected
        .iter()
        .all(|e| matches!(&e.kind, EventKind::Rejected { reason, .. } if reason == "the broker rejected the order: no buying power")));
    assert!(r.fills.is_empty() && r.final_positions.is_empty());
    // Acknowledged, then expired by its time in force.
    let r = run(|_, asked| if asked == 0 { vec![Report::New] } else { vec![Report::Expired { reason: "day order".into() }] });
    let expired = of_kind(&r, "expired_order");
    assert_eq!(expired.len(), 2);
    assert!(expired
        .iter()
        .all(|e| e.t == r.bars[2] && matches!(&e.kind, EventKind::ExpiredOrder { reason, .. } if reason == "expired at the broker: day order")));
    // Cancelled by the broker.
    let r = run(|_, asked| if asked == 0 { vec![Report::New] } else { vec![Report::Cancelled { reason: "risk desk".into() }] });
    let dropped = of_kind(&r, "dropped");
    assert_eq!(dropped.len(), 2);
    assert!(dropped
        .iter()
        .all(|e| matches!(&e.kind, EventKind::Dropped { reason, .. } if reason == "cancelled by the broker: risk desk")));
}

/// A decision on an instrument whose order still works replaces it at the
/// broker; what works at the run's end is cancelled there.
#[test]
fn a_new_decision_replaces_the_working_order_and_the_end_cancels_it() {
    let prog = halves();
    let ds = two_names();
    let cfg = ExecConfig::from_program(&prog);
    let client = common::client(&prog, &ds);
    // Acknowledged, never filled.
    let mock = MockBroker::with_policy(cfg.initial_cash, |_, _| vec![Report::New]);
    let r = broker_run(&prog, &client, cfg, &mock, &deployment(""), Replay::default()).unwrap();
    let bars = r.bars.len();
    // Two names, a decision each at every bar: every order but the last
    // bar's is replaced by the next bar's, the last bar's cancelled at the
    // end.
    assert_eq!(of_kind(&r, "replaced").len(), 2 * (bars - 1));
    assert_eq!(mock.replaced().len(), 2 * (bars - 1));
    let ended: Vec<_> = of_kind(&r, "dropped");
    assert_eq!(ended.len(), 2);
    assert!(ended
        .iter()
        .all(|e| matches!(&e.kind, EventKind::Dropped { reason, .. } if reason == "the run ended: cancelled at the broker")));
    assert_eq!(mock.cancelled().len(), 2);
    assert!(mock.working().is_empty(), "the run leaves nothing working");
    assert!(r.fills.is_empty());
    accounted(&r);
}

/// A report the lifecycle does not admit (a second fill of a filled order)
/// is ignored, and the run says so.
#[test]
fn a_report_the_lifecycle_does_not_admit_is_ignored_and_warned() {
    let prog = buy_once();
    let ds = two_names();
    let cfg = ExecConfig::from_program(&prog);
    let client = common::client(&prog, &ds);
    let mock = MockBroker::with_policy(cfg.initial_cash, |o, _| {
        let f = Report::Filled {
            quantity: o.quantity,
            price: o.reference.unwrap(),
            commission: 0.0,
        };
        vec![Report::New, f.clone(), f]
    });
    let r = broker_run(&prog, &client, cfg, &mock, &deployment("reconcile warn"), Replay::default()).unwrap();
    assert_eq!(r.final_positions.get(&sym(&r, "AAA")).copied(), Some(100.0), "one fill of the hundred");
    assert!(
        r.warnings
            .iter()
            .any(|w| w.bias == "broker" && w.message.contains("2 broker reports ignored") && w.message.contains("cannot report `filled`")),
        "{:?}",
        r.warnings
    );
    // The account took both: the book and the broker are apart.
    assert!(r.warnings.iter().any(|w| w.bias == "reconciliation" && w.message.contains("reconcile warn")), "{:?}", r.warnings);
    accounted(&r);
}

// --------------------------------------------------- reconciliation

#[test]
fn reconcile_halt_stops_the_run_at_the_sessions_open() {
    let prog = halves();
    let ds = two_names();
    let cfg = ExecConfig::from_program(&prog);
    let client = common::client(&prog, &ds);
    let mock = MockBroker::new(cfg.initial_cash);
    mock.set_position("AAA", 7.0);
    let (out, id) = broker_run_named(&prog, &client, cfg, &mock, &deployment(""), Replay::default());
    match out {
        Err(RunError::Risk { t, rule, decision, message }) => {
            assert_eq!(t, common::day("2024-01-08"));
            assert_eq!((rule.as_str(), decision.as_str()), ("(reconciliation)", "(session_open)"));
            assert!(message.contains("AAA 0 in the book, 7 at the broker") && message.contains("reconcile halt"), "{}", message);
        }
        other => panic!("the run should halt: {:?}", other.map(|r| r.run_id)),
    }
    // The halted run wrote what it had: the reconciliation event.
    let events = scan(&client, &format!("kernel.{}.events", id));
    let kinds = texts(&events, "kind");
    assert!(kinds.iter().any(|k| k.as_deref() == Some("reconciliation")), "{:?}", kinds);
    assert!(mock.sent().is_empty(), "nothing was sent");
}

#[test]
fn reconcile_adopt_takes_the_brokers_account() {
    let prog = halves();
    let ds = two_names();
    let cfg = ExecConfig::from_program(&prog);
    let client = common::client(&prog, &ds);
    let mock = MockBroker::new(cfg.initial_cash);
    mock.set_position("AAA", 7.0);
    mock.set_cash(cfg.initial_cash - 70.0);
    let r = broker_run(&prog, &client, cfg.clone(), &mock, &deployment("reconcile adopt"), Replay::default()).unwrap();
    let (aaa, first) = (sym(&r, "AAA"), r.bars[0]);
    let rec = of_kind(&r, "reconciliation");
    assert_eq!(rec.len(), 1, "once adopted the book is the account");
    assert_eq!(rec[0].t, first);
    assert_eq!(rec[0].cash_flow, Some(-70.0));
    match &rec[0].kind {
        EventKind::Reconciliation { policy, at, positions, cash, adopted } => {
            assert_eq!((policy.as_str(), at.as_str(), *adopted), ("adopt", "session_open", true));
            assert_eq!(positions, &vec![(aaa, 0.0, 7.0)]);
            assert_eq!(*cash, (cfg.initial_cash, cfg.initial_cash - 70.0));
        }
        k => panic!("{:?}", k),
    }
    // The first bar's book holds the broker's seven, and its order is sized
    // against them.
    assert!(r.holdings.iter().any(|h| h.t == first && h.instrument == aaa && h.position == 7.0));
    let (_, sent) = mock.sent().into_iter().find(|(_, o)| o.instrument == "AAA").unwrap();
    assert_eq!(sent.quantity, 50_000.0 - 7.0);
    assert_eq!(r.final_cash, mock.account().cash);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.bias == "reconciliation" && w.message.contains("adopted the broker's account at 1 reconciliation (")),
        "{:?}",
        r.warnings
    );
    accounted(&r);
}

#[test]
fn reconcile_warn_reports_and_keeps_the_book() {
    let prog = halves();
    let ds = two_names();
    let cfg = ExecConfig::from_program(&prog);
    let client = common::client(&prog, &ds);
    let mock = MockBroker::new(cfg.initial_cash);
    mock.set_position("BBB", 3.0);
    let r = broker_run(&prog, &client, cfg, &mock, &deployment("reconcile warn"), Replay::default()).unwrap();
    // Every bar of daily bars is a session's open, and the book never
    // takes the three.
    let rec = of_kind(&r, "reconciliation");
    assert_eq!(rec.len(), r.bars.len());
    assert!(rec
        .iter()
        .all(|e| matches!(&e.kind, EventKind::Reconciliation { adopted: false, policy, .. } if policy == "warn") && e.cash_flow.is_none()));
    let bbb = sym(&r, "BBB");
    assert_eq!(mock.account().positions.get("BBB").copied().unwrap() - r.final_positions[&bbb], 3.0);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.bias == "reconciliation" && w.message.contains(&format!("at {} reconciliations (reconcile warn)", r.bars.len()))),
        "{:?}",
        r.warnings
    );
    accounted(&r);
}

/// The session drops after the second bar's orders and comes back two bars
/// later, the account changed meanwhile: orders while it is down are
/// refused, what was working fills when it is back, and the reconnect is
/// reconciled.
#[test]
fn a_reconnect_is_reconciled_and_orders_while_down_are_refused() {
    let prog = halves();
    let ds = two_names();
    let cfg = ExecConfig::from_program(&prog);
    let client = common::client(&prog, &ds);
    let mock = MockBroker::new(cfg.initial_cash);
    // `events` is asked at every bar from the second: its second call is
    // the third bar's.
    mock.at_call(1, Scripted::Disconnect);
    mock.at_call(2, Scripted::Reconnect);
    mock.at_call(2, Scripted::Position("BBB".into(), 1.0));
    let r = broker_run(&prog, &client, cfg, &mock, &deployment("reconcile warn"), Replay::default()).unwrap();
    let (b3, b4) = (r.bars[2], r.bars[3]);
    let refused = of_kind(&r, "rejected");
    assert_eq!(refused.len(), 2);
    assert!(refused
        .iter()
        .all(|e| e.t == b3 && matches!(&e.kind, EventKind::Rejected { reason, .. } if reason.contains("session is down"))));
    let rec: Vec<_> = of_kind(&r, "reconciliation")
        .into_iter()
        .filter(|e| matches!(&e.kind, EventKind::Reconciliation { at, .. } if at == "reconnect"))
        .collect();
    assert_eq!(rec.len(), 1);
    assert_eq!(rec[0].t, b4);
    assert!(r.warnings.iter().any(|w| w.bias == "broker" && w.message.contains("went down once")), "{:?}", r.warnings);
    // The second bar's orders worked through the drop and filled at the
    // fourth bar.
    assert!(r.fills.iter().any(|f| f.t == b4));
    accounted(&r);
}

#[test]
fn a_broker_down_past_the_session_deadline_halts_the_run() {
    let prog = halves();
    let ds = two_names();
    let cfg = ExecConfig::from_program(&prog);
    let client = common::client(&prog, &ds);
    let mock = MockBroker::new(cfg.initial_cash);
    mock.at_call(1, Scripted::Disconnect);
    match broker_run(&prog, &client, cfg, &mock, &deployment("deadline { session 1min }"), Replay::default()) {
        Err(RunError::Risk { t, rule, message, .. }) => {
            assert_eq!(rule, "(broker)");
            assert_eq!(t, common::day("2024-01-11"), "the bar after the drop");
            assert!(message.contains("down since 2024-01-10") && message.contains("60 s"), "{}", message);
        }
        other => panic!("{:?}", other.map(|r| r.run_id)),
    }
}

/// The broker holds an instrument the run does not know: `adopt` cannot
/// take it.
#[test]
fn adopt_halts_on_an_instrument_the_run_does_not_know() {
    let prog = halves();
    let ds = two_names();
    let cfg = ExecConfig::from_program(&prog);
    let client = common::client(&prog, &ds);
    let mock = MockBroker::new(cfg.initial_cash);
    mock.set_position("ZZZ", 5.0);
    match broker_run(&prog, &client, cfg, &mock, &deployment("reconcile adopt"), Replay::default()) {
        Err(RunError::Risk { rule, message, .. }) => {
            assert_eq!(rule, "(reconciliation)");
            assert!(message.contains("ZZZ 5 at the broker, an instrument the run does not know"), "{}", message);
        }
        other => panic!("{:?}", other.map(|r| r.run_id)),
    }
}

// ------------------------------------------------------ the kill switch

/// `halt_all` by a control request (what the kernel service's `control
/// halt` sets): the run stops after the barrier it was asked at, the
/// control logged.
#[test]
fn halt_all_stops_the_run_after_the_barrier() {
    let prog = halves();
    let ds = two_names();
    let cfg = ExecConfig::from_program(&prog);
    let client = common::client(&prog, &ds);
    let control = LiveControl::new();
    let asked = control.clone();
    let mut n = 0;
    // The control is asked for as the third bar's reports come in.
    let mock = MockBroker::with_policy(cfg.initial_cash, move |o, k| {
        n += 1;
        if n == 3 {
            asked.halt();
        }
        MockBroker::fill_at_reference(o, k)
    });
    let opts = Replay {
        control: Some(control),
        ..Replay::default()
    };
    let (out, id) = broker_run_named(&prog, &client, cfg, &mock, &deployment(""), opts);
    match out {
        Err(RunError::Risk { t, rule, decision, message }) => {
            assert_eq!(t, common::day("2024-01-10"));
            assert_eq!((rule.as_str(), decision.as_str()), ("(kill switch)", "(halt_all)"));
            assert!(message.contains("(control)"), "{}", message);
        }
        other => panic!("{:?}", other.map(|r| r.run_id)),
    }
    let events = scan(&client, &format!("kernel.{}.events", id));
    let (kinds, reasons) = (texts(&events, "kind"), texts(&events, "reason"));
    let control: Vec<_> = kinds.iter().zip(&reasons).filter(|(k, _)| k.as_deref() == Some("control")).collect();
    assert_eq!(control.len(), 1);
    assert_eq!(control[0].1.as_deref(), Some("halt_all"));
    // The barrier it was asked at decided and sent its orders.
    assert_eq!(mock.sent().len(), 6);
}

/// `halt_all file PATH`: the file there halts the run after its first
/// barrier.
#[test]
fn halt_all_on_a_file() {
    let dir = std::env::temp_dir().join(format!("abt-m10-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("halt");
    let d = deployment(&format!("kill_switch {{ halt_all file \"{}\" }}", path.display()));
    assert_eq!(d.halt_all, vec![HaltSource::File(path.display().to_string())]);
    let control = LiveControl::of(&d);
    assert_eq!(control.halt_requested(), None);
    std::fs::write(&path, "").unwrap();
    assert_eq!(control.halt_requested(), Some(format!("file {}", path.display())));
    let prog = halves();
    let ds = two_names();
    let cfg = ExecConfig::from_program(&prog);
    let client = common::client(&prog, &ds);
    let mock = MockBroker::new(cfg.initial_cash);
    match broker_run(&prog, &client, cfg, &mock, &d, Replay::default()) {
        Err(RunError::Risk { t, rule, message, .. }) => {
            assert_eq!((t, rule.as_str()), (common::day("2024-01-08"), "(kill switch)"));
            assert!(message.contains(&format!("file {}", path.display())), "{}", message);
        }
        other => panic!("{:?}", other.map(|r| r.run_id)),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `flatten_all` through decisions: at the bar it is asked at, the
/// program's decisions give way to a closing decision on every position,
/// logged, sent and filled like any; the run goes on.
#[test]
fn flatten_all_closes_every_position_through_decisions() {
    let prog = halves();
    let ds = two_names();
    let cfg = ExecConfig::from_program(&prog);
    let client = common::client(&prog, &ds);
    let control = LiveControl::new();
    let asked = control.clone();
    let mut n = 0;
    let mock = MockBroker::with_policy(cfg.initial_cash, move |o, k| {
        n += 1;
        if n == 5 {
            asked.flatten();
        }
        MockBroker::fill_at_reference(o, k)
    });
    let opts = Replay {
        control: Some(control),
        ..Replay::default()
    };
    let r = broker_run(&prog, &client, cfg, &mock, &deployment(""), opts).unwrap();
    let (f, after) = (r.bars[3], r.bars[4]);
    let ctl = of_kind(&r, "control");
    assert_eq!(ctl.len(), 1);
    assert_eq!(ctl[0].t, f);
    assert!(matches!(&ctl[0].kind, EventKind::Control { action, source } if action == "flatten_all" && source == "control"));
    // The program's decisions at the bar are dropped before they are
    // orders.
    let dropped: Vec<_> = of_kind(&r, "dropped").into_iter().filter(|e| e.t == f).collect();
    assert_eq!(dropped.len(), 2);
    assert!(dropped
        .iter()
        .all(|e| e.order_id.is_none() && matches!(&e.kind, EventKind::Dropped { reason, .. } if reason.starts_with("flatten_all"))));
    // The closing decisions, the control's, on what the book held.
    let held = Book::fold(1_000_000.0, &r.events[..ctl[0].seq as usize]);
    let closing: Vec<_> = r
        .events
        .iter()
        .filter(|e| e.t == f && e.rule == Some(NO_RULE) && matches!(&e.kind, EventKind::Decision { decision } if decision.ctor == Ctor::HoldQuantity && decision.amount == 0.0))
        .collect();
    assert_eq!(closing.len(), held.positions.len());
    assert_eq!(held.positions.len(), 2);
    // Sent and filled at the next bar: the book flat there before the
    // program buys again.
    let orders: Vec<u64> = r
        .events
        .iter()
        .filter(|e| e.t == f && e.rule == Some(NO_RULE) && e.kind.name() == "order_sent")
        .filter_map(|e| e.order_id)
        .collect();
    assert_eq!(orders.len(), 2);
    for e in r.events.iter().filter(|e| e.order_id.is_some_and(|o| orders.contains(&o))) {
        if let EventKind::Fill { fill, .. } = &e.kind {
            assert_eq!(e.t, after);
            assert_eq!(fill.quantity, -held.positions[&fill.equity]);
        }
    }
    let flat = Book::fold(1_000_000.0, &r.events[..=r.events.iter().rposition(|e| e.order_id.is_some_and(|o| orders.contains(&o))).unwrap()]);
    assert!(flat.positions.is_empty(), "{:?}", flat.positions);
    assert!(r.final_positions.len() == 2, "the program bought again");
    accounted(&r);
}

// ----------------------------------------------------- the deployment

const DOCUMENT: &str = "deployment ibkr_paper {
  account DU1234567   broker ibkr
  session { host 127.0.0.1, port 4002, sender ABT, target IBKR, version FIX.4.4 }
  routing destination SMART   tif_map { market -> DAY, moc -> MOC, limit -> GTC }
  kill_switch { halt_all on SIGTERM | file /run/abt/halt | socket; flatten_all through decisions }
  reconcile adopt at session_open, reconnect   cash_tolerance 0.5
  deadline { base 55s; session 5min }
  recording stream live-equities
}
";

#[test]
fn the_deployment_document_parses_and_hashes_its_content() {
    let d = deployment::parse(DOCUMENT).unwrap();
    assert_eq!(d.name.as_deref(), Some("ibkr_paper"));
    assert_eq!((d.account.as_str(), d.broker.as_str()), ("DU1234567", "ibkr"));
    assert_eq!(d.session.get("port").map(String::as_str), Some("4002"));
    assert_eq!(d.session.get("version").map(String::as_str), Some("FIX.4.4"));
    assert_eq!(d.destination.as_deref(), Some("SMART"));
    assert_eq!(d.tif_map.get("moc").map(String::as_str), Some("MOC"));
    let moc = abt_kernel::kernel::value::Order {
        kind: abt_kernel::kernel::value::OrderKind::Moc,
        tif: Default::default(),
    };
    assert_eq!(d.tif_for(&moc).as_deref(), Some("MOC"));
    assert_eq!(d.tif_for(&abt_kernel::kernel::value::Order::default()).as_deref(), Some("DAY"));
    assert_eq!(d.halt_all, vec![HaltSource::Signal("SIGTERM".into()), HaltSource::File("/run/abt/halt".into()), HaltSource::Socket]);
    assert_eq!(d.reconcile, Policy::Adopt);
    assert_eq!(d.reconcile_at, vec!["session_open".to_string(), "reconnect".to_string()]);
    assert_eq!(d.cash_tolerance, 0.5);
    assert_eq!((d.deadline_base, d.deadline_session), (Some(55_000_000_000), Some(300_000_000_000)));
    assert_eq!(d.recording.as_deref(), Some("live-equities"));
    // The hash is of the content: layout and comments do not change it, a
    // field does.
    let relaid = "# the paper account\ndeployment ibkr_paper { recording stream live-equities\n deadline { session 5min, base 55s }  reconcile adopt at session_open,reconnect cash_tolerance 0.5\n kill_switch { halt_all on TERM | file \"/run/abt/halt\" | socket ; flatten_all through decisions }\n routing { tif_map { limit->GTC, moc -> MOC, market -> DAY } destination SMART }\n session { version FIX.4.4 target IBKR sender ABT port 4002 host 127.0.0.1 } broker ibkr account DU1234567 }";
    assert_eq!(deployment::parse(relaid).unwrap(), d);
    assert_eq!(deployment::parse(relaid).unwrap().hash(), d.hash());
    assert_ne!(deployment::parse(&DOCUMENT.replace("reconcile adopt", "reconcile warn")).unwrap().hash(), d.hash());
    assert_eq!(d.hash().len(), 64);
    // The defaults: halt at both events, a cent, the control request.
    let d = deployment("");
    assert_eq!((d.reconcile, d.cash_tolerance), (Policy::Halt, 0.01));
    assert!(d.reconciles_at("session_open") && d.reconciles_at("reconnect"));
    assert_eq!(d.halt_all, vec![HaltSource::Socket]);
}

#[test]
fn a_deployment_document_that_does_not_parse_says_why() {
    let refused = [
        ("deployment { broker mock }", "names its `account`"),
        ("deployment { account A }", "names its `broker`"),
        ("deployment { account A broker mock acount B }", "`acount` is not a field of a deployment"),
        ("deployment { account A broker mock reconcile maybe }", "`maybe` is not halt, adopt or warn"),
        ("deployment { account A broker mock reconcile warn at midnight }", "it runs at session_open and reconnect"),
        ("deployment { account A broker mock kill_switch { halt_all on SIGFOO } }", "not a signal the kill switch takes"),
        ("deployment { account A broker mock kill_switch { flatten_all now } }", "flatten_all goes `through decisions`"),
        ("deployment { account A broker mock deadline { base soon } }", "is not a duration"),
        ("deployment { account A broker mock session { port } }", "the value of session's `port`"),
        ("deployment { account A broker mock tif_map { market DAY } }", "expected `->`"),
        ("deployment { account A broker mock tif_map { iceberg -> X } }", "`iceberg` is not an order kind"),
        ("deployment { account A broker mock account B }", "`account` is given twice"),
        ("deployment { account A broker mock", "does not close"),
        ("deployment { account A broker mock } more", "text after the deployment's closing"),
        ("account A broker mock", "starts with `deployment`"),
    ];
    for (text, why) in refused {
        let e = deployment::parse(text).expect_err(text);
        assert!(e.contains(why), "{}: {}", text, e);
    }
}

#[test]
fn a_broker_without_an_adapter_is_refused_and_the_mock_runs_from_the_mode() {
    let d = deployment::parse(DOCUMENT).unwrap();
    let e = live::connect(&d, 1e6).err().expect("ibkr has no adapter here");
    assert!(e.to_string().contains("broker `ibkr`: this build has no adapter for it"), "{}", e);
    let cfg = ExecConfig::frictionless();
    let opts = |d: Option<Deployment>| Replay {
        mode: RunMode::LiveBroker { session: None },
        deployment: d,
        ..Replay::default()
    };
    assert!(executor_for(&cfg, &opts(Some(d))).is_err());
    assert!(executor_for(&cfg, &opts(None)).err().unwrap().to_string().contains("takes its deployment document"));
    // The mode alone: `run_on` takes the deployment's broker, the mock.
    let prog = buy_once();
    let ds = two_names();
    let client = common::client(&prog, &ds);
    let r = run_on(&prog, None, &client, ExecConfig::from_program(&prog), &opts(Some(deployment("")))).unwrap();
    assert_eq!(r.mode, "live-broker");
    assert_eq!(r.fills.len(), 2);
    assert!(r.fills.iter().all(|f| f.broker.is_some()));
    // A replay of a recorded run is replay-sim's, or a rehearsal's.
    let e = run_on(
        &prog,
        None,
        &client,
        ExecConfig::from_program(&prog),
        &Replay {
            mode: RunMode::LiveSim { session: "s".into() },
            replay_of: Some("r".into()),
            ..Replay::default()
        },
    )
    .unwrap_err();
    assert!(e.to_string().contains("a live-broker rehearsal without a session"), "{}", e);
}

/// What the run writes (K-7.1): the binding records the mode and the
/// deployment's hash, the fills their source, the events the broker's
/// order ids.
#[test]
fn the_outputs_record_the_mode_the_deployment_and_the_brokers_fills() {
    let prog = buy_once();
    let ds = two_names();
    let cfg = ExecConfig::from_program(&prog);
    let client = common::client(&prog, &ds);
    let d = deployment("reconcile warn");
    let mock = MockBroker::new(cfg.initial_cash);
    let r = broker_run(
        &prog,
        &client,
        cfg.clone(),
        &mock,
        &d,
        Replay {
            write_outputs: true,
            ..Replay::default()
        },
    )
    .unwrap();
    assert_eq!(r.deployment_hash.as_deref(), Some(d.hash().as_str()));
    let binding = scan(&client, &format!("kernel.{}.binding", r.run_id));
    assert!(texts(&binding, "mode").iter().all(|m| m.as_deref() == Some("live-broker")));
    assert!(texts(&binding, "deployment_hash").iter().all(|h| h.as_deref() == Some(d.hash().as_str())));
    let f = scan(&client, &format!("kernel.{}.fills", r.run_id));
    assert_eq!(f.num_rows(), 2);
    assert!(texts(&f, "source").iter().all(|s| s.as_deref() == Some("broker")));
    let events = scan(&client, &format!("kernel.{}.events", r.run_id));
    let details: Vec<String> = texts(&events, "detail").into_iter().flatten().filter(|d| d.contains("broker_order")).collect();
    assert_eq!(details.len(), 2, "{:?}", details);
    // A simulated run's binding: replay-sim, no deployment.
    let sim = sim_run(
        &prog,
        &client,
        cfg,
        Replay {
            write_outputs: true,
            ..Replay::default()
        },
    );
    let binding = scan(&client, &format!("kernel.{}.binding", sim.run_id));
    assert!(texts(&binding, "mode").iter().all(|m| m.as_deref() == Some("replay-sim")));
    assert!(texts(&binding, "deployment_hash").iter().all(|h| h.is_none()));
    assert!(texts(&scan(&client, &format!("kernel.{}.fills", sim.run_id)), "source").iter().all(|s| s.as_deref() == Some("sim")));
}
