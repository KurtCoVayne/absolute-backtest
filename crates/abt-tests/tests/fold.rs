//! The fold over the data layer's stream (K-4.2): a run in process equals
//! the run over a Unix socket to the bit on every corpus strategy and on
//! crafted markets (Proposition P-5.1, G3); a stream truncated at a decision
//! barrier decides as the whole run did up to that bar (Proposition K-4.1,
//! G5, the causality theorem operationally); and the stream carries every
//! tuple of the primitives the program reads, in receipt order, with the
//! barriers of its calendars.

use abt_tests as common;

use abt_kernel::check::Program;
use abt_kernel::data::{synthetic_daily, synthetic_minute};
use abt_kernel::kernel::bind::{bind, stream_spec, Decoder, StreamEvent};
use abt_kernel::kernel::{Dataset, ExecConfig, Kernel, Replay, RunResult, Value};
use abt_kernel::Kind;
use abt_tests::{run, run_socket};

fn corpus_strategies() -> Vec<(String, Program)> {
    common::dsl_files("strategies")
        .into_iter()
        .map(|f| {
            let name = f.file_stem().unwrap().to_string_lossy().to_string();
            (name.clone(), common::corpus_program(&name))
        })
        .collect()
}

/// The names of the synthetic markets, in the order the acceptance test
/// uses: the daily market renames its second name halfway and delists its
/// last, so the tickers the strategies cite (`AAA`, `BBB`, `SPY`) stay.
const SYMS: [&str; 5] = ["AAA", "XYZ", "BBB", "SPY", "DDD"];

/// The synthetic market of a program's base: minute bars for a sub-daily
/// base, else the daily market with its catalog. Four hundred daily bars,
/// so that every strategy, the twelve-month momentum included, decides.
fn dataset_for(p: &Program, seed: u64) -> Dataset {
    if p.base.is_sub_daily() {
        synthetic_minute(&SYMS, (2024, 1, 2), 30, 320, seed)
    } else {
        synthetic_daily(&SYMS, (2022, 1, 3), 400, seed)
    }
}

/// Everything a run produced, as comparable text and bits.
fn digest(r: &RunResult) -> Vec<String> {
    let mut out = Vec::new();
    out.push(format!("bars {:?}", r.bars));
    for d in &r.decisions {
        out.push(format!("decision {} {} {}", d.t, r.describe_decision(&d.decision), d.rule));
    }
    for f in &r.fills {
        out.push(format!(
            "fill {} {} {:x} {:x} {} {:x} {:x} {:x} {:x} {:x} {} {}",
            f.t,
            r.symbols[f.equity as usize],
            f.quantity.to_bits(),
            f.price.to_bits(),
            f.at_last_price,
            f.commission.to_bits(),
            f.fee.to_bits(),
            f.slippage.to_bits(),
            f.impact.to_bits(),
            f.participation.to_bits(),
            f.partial,
            f.forced
        ));
    }
    for (t, d, why) in &r.dropped {
        out.push(format!("dropped {} {} {}", t, r.describe_decision(d), why));
    }
    for (t, e) in &r.equity_curve {
        out.push(format!("equity {} {:x}", t, e.to_bits()));
    }
    for e in &r.exposure {
        out.push(format!(
            "exposure {} {:x} {:x} {:x} {:x}",
            e.t,
            e.cash.to_bits(),
            e.gross.to_bits(),
            e.net.to_bits(),
            e.leverage.to_bits()
        ));
    }
    for a in &r.actions {
        out.push(format!("action {} {} {:?} {:x}", a.t, r.symbols[a.equity as usize], a.action, a.cash.to_bits()));
    }
    out.push(format!("costs {:?}", r.costs));
    out.push(format!("liquidity {:?}", r.liquidity));
    out.push(format!("funding {:?}", r.funding));
    out.push(format!("warnings {:?}", r.warnings));
    out.push(format!("final_cash {:x}", r.final_cash.to_bits()));
    out.push(format!("final_positions {:?}", r.final_positions));
    out
}

fn assert_same(name: &str, a: &RunResult, b: &RunResult) {
    let (da, db) = (digest(a), digest(b));
    if da != db {
        let first = da.iter().zip(&db).position(|(x, y)| x != y).unwrap_or(da.len().min(db.len()));
        panic!(
            "{}: in process and over a socket differ at line {} of {} / {}:\n  in process: {}\n  socket:     {}",
            name,
            first,
            da.len(),
            db.len(),
            da.get(first).cloned().unwrap_or_default(),
            db.get(first).cloned().unwrap_or_default()
        );
    }
}

/// The run in process against the run over a socket on the corpus
/// strategies whose base matches `sub_daily`, every one of which must decide
/// something.
fn inproc_equals_socket(sub_daily: bool) {
    let mut seen = 0;
    for (name, p) in corpus_strategies() {
        if p.base.is_sub_daily() != sub_daily {
            continue;
        }
        seen += 1;
        let ds = dataset_for(&p, 11);
        let cfg = ExecConfig::from_program(&p);
        let a = run(&p, &ds, cfg.clone()).unwrap_or_else(|e| panic!("{}: {}", name, e));
        let b = run_socket(&p, &ds, cfg).unwrap_or_else(|e| panic!("{} (socket): {}", name, e));
        assert_same(&name, &a, &b);
        assert!(!b.decisions.is_empty(), "{} decided nothing", name);
    }
    assert!(seen > 0);
}

#[test]
fn a_run_in_process_equals_the_run_over_a_socket_on_every_daily_corpus_strategy() {
    inproc_equals_socket(false);
}

/// The strategies on a minute base (`opening_gap`, `resampled_momentum`),
/// whose daily catalog is keyed at the date.
#[test]
fn a_run_in_process_equals_the_run_over_a_socket_on_every_minute_corpus_strategy() {
    inproc_equals_socket(true);
}

#[test]
fn a_run_in_process_equals_the_run_over_a_socket_on_crafted_markets() {
    // Missing prices, a liquidation at the last price, a delisting, a split
    // and a dividend: the crafted cases of the executor tests. The name is
    // in the universe while listed, so that a bar with no price is still a
    // bar at which its position is decided on.
    let round_trip = common::StrategyParts {
        name: "round_trip".into(),
        universe: "universe(A, T) :- listed(A, T).".into(),
        params: "    qty : Quantity<Shares> = 10 shares".into(),
        decides: "    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T).\n    decide(T, trade(A, Q)) :- held(A, T, Q0), Q = -Q0.".into(),
        ..Default::default()
    }
    .render();
    let p = common::program(&round_trip, "round_trip");
    let mut ds = Dataset::new();
    let x = ds.add_security("X", "X", common::day("2024-01-08"), None);
    for (d, price) in [("2024-01-08", Some(10.0)), ("2024-01-09", Some(12.0)), ("2024-01-10", None), ("2024-01-11", None), ("2024-01-12", None)] {
        let t = common::day(d);
        if let Some(p) = price {
            for rel in ["open", "high", "low", "close"] {
                ds.add(rel, vec![Value::Equity(x), Value::Time(t), Value::Num(p)]);
            }
        }
        ds.add("volume", vec![Value::Equity(x), Value::Time(t), Value::Num(1000.0)]);
        ds.add("listed", vec![Value::Equity(x), Value::Time(t)]);
    }
    ds.derive_tickers();
    let cfg = ExecConfig {
        initial_cash: 1000.0,
        ..ExecConfig::frictionless()
    };
    let batch = run(&p, &ds, cfg.clone()).unwrap();
    assert_same("round_trip", &batch, &run_socket(&p, &ds, cfg).unwrap());
    assert!(!batch.decisions.is_empty());
    // The catalog market under the execution block, with a margin preset
    // and a tight participation cap too.
    let p = common::corpus_program("total_return_momentum");
    let ds = synthetic_daily(&SYMS, (2022, 1, 3), 400, 5);
    let block = ExecConfig::from_program(&p);
    for cfg in [
        block.clone(),
        ExecConfig::reg_t(),
        ExecConfig {
            participation_cap: 0.001,
            on_leverage: abt_kernel::kernel::OnLeverage::Reject,
            ..block
        },
    ] {
        assert_same("total_return_momentum", &run(&p, &ds, cfg.clone()).unwrap(), &run_socket(&p, &ds, cfg).unwrap());
    }
}

#[test]
fn a_stream_truncated_at_a_decision_barrier_decides_as_the_whole_run() {
    let p = common::corpus_program("momentum_top_n");
    let ds = synthetic_daily(&SYMS, (2022, 1, 3), 200, 7);
    let cfg = ExecConfig::from_program(&p);
    let client = common::client(&p, &ds);
    let full = abt_kernel::kernel::run_on(&p, None, &client, cfg.clone(), &Replay::default()).unwrap();
    let bars = full.bars.clone();
    for &cut in &[bars[60], bars[133], bars[199]] {
        let early = abt_kernel::kernel::run_on(
            &p,
            None,
            &client,
            cfg.clone(),
            &Replay {
                horizon: Some(cut),
                ..Replay::default()
            },
        )
        .unwrap();
        assert_eq!(*early.bars.last().unwrap(), cut, "the stream ends after the barrier of {}", cut);
        assert_eq!(early.bars, bars.iter().copied().filter(|b| *b <= cut).collect::<Vec<_>>());
        for &t in early.bars.iter() {
            let d = |r: &RunResult| -> Vec<String> { r.decisions_at(t).into_iter().map(|d| r.describe_decision(d)).collect() };
            assert_eq!(d(&early), d(&full), "decisions at {} with the stream cut at {}", t, cut);
        }
        // The book before the cut bar is the whole run's.
        let before = |r: &RunResult| -> Vec<(i64, u64)> { r.equity_curve.iter().filter(|(t, _)| *t < cut).map(|(t, e)| (*t, e.to_bits())).collect() };
        assert_eq!(before(&early), before(&full));
    }
}

#[test]
fn the_stream_carries_the_programs_primitives_in_receipt_order_with_its_barriers() {
    let p = common::corpus_program("sma_crossover");
    let ds = synthetic_daily(&SYMS, (2022, 1, 3), 20, 1);
    let cfg = ExecConfig::from_program(&p);
    let client = common::client(&p, &ds);
    let bound = bind(&p, &client).unwrap();
    let mut k = Kernel::bound(&p, &bound, cfg).unwrap();
    assert!(k.decision_bars().is_empty(), "a bound kernel starts with no bars");
    let (spec, cal_of) = stream_spec(&k, &bound, &client, "test").unwrap();
    let mut decoder = Decoder::new(&k, &bound, cal_of);
    let mut sub = client.subscribe(spec, 1, None).unwrap();
    let mut events = Vec::new();
    while let Some(b) = sub.next_batch().unwrap() {
        events.extend(decoder.events(&mut k, &bound, &b).unwrap());
    }
    // Gap-free from 1; tuples in (receipt time, relation) order; a barrier
    // of the decision calendar at every bar.
    let seqs: Vec<u64> = events
        .iter()
        .map(|e| match e {
            StreamEvent::Tuple { seq, .. } | StreamEvent::Barrier { seq, .. } => *seq,
        })
        .collect();
    assert_eq!(seqs, (1..=events.len() as u64).collect::<Vec<_>>());
    let keys: Vec<(i64, usize)> = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::Tuple { r, rel, .. } => Some((*r, *rel)),
            _ => None,
        })
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
    let barriers = events.iter().filter(|e| matches!(e, StreamEvent::Barrier { cal, .. } if *cal == p.decision_cal)).count();
    assert_eq!(barriers, 20);
    // Every tuple of a primitive the program reads (the bars, the actions,
    // the listing and the tickers of five names over twenty bars), once;
    // the catalog the demand does not ask for (the index membership) is
    // not in the stream.
    let expected: usize = ds
        .facts
        .iter()
        .filter(|(name, _)| matches!(p.relations.get(name.as_str()).map(|s| &s.kind), Some(Kind::Primitive { .. })))
        .map(|(_, tuples)| tuples.len())
        .sum();
    assert!(
        ds.facts.contains_key("member") && !p.relations.contains_key("member"),
        "the market has a membership the demand does not read"
    );
    assert_eq!(keys.len(), expected);
    assert!(expected >= 7 * 5 * 16, "open, high, low, close, volume, listed and ticker: {}", expected);
}
