//! Availability time per tuple (data-bundle doc, sections 2 and 4, and the
//! semantic model's v2 item 1): a tuple is received at its own receipt time
//! `r` (D-4.2), which may be after its bar's close; the stream places it
//! after the barriers before `r` (D-5.2), the fold reads it from then on,
//! nothing is rewound, and the causality theorem is judged on receipt: the
//! decisions at t of the whole stream equal those of the stream truncated at
//! t's barrier (G5).

use abt_tests as common;

use std::collections::BTreeMap;
use std::fs;

use abt_kernel::check::Program;
use abt_kernel::data::{business_days, synthetic_daily, write_parquet_dir};
use abt_kernel::kernel::bind::{bind, stream_spec, Decoder, StreamEvent};
use abt_kernel::kernel::time::{format_timestamp, DAY};
use abt_kernel::kernel::{run_on, Dataset, Event, ExecConfig, Fold, Kernel, Replay, SimExecutor, Value};
use abt_tests::{run, run_socket, verify_causality};

const SYMS: [&str; 5] = ["AAA", "BBB", "CCC", "DDD", "SPY"];

/// The synthetic market, every close received at its bar.
fn on_time() -> Dataset {
    synthetic_daily(&SYMS, (2022, 1, 3), 200, 7)
}

/// The synthetic market with every tenth close of each name available one
/// bar late (at the next bar's close).
fn delayed() -> Dataset {
    let base = on_time();
    let mut bars: Vec<i64> = base.facts["close"].iter().map(|tu| tu[1].as_time().unwrap()).collect();
    bars.sort();
    bars.dedup();
    let mut ds = Dataset {
        facts: BTreeMap::new(),
        available_at: BTreeMap::new(),
        ..base.clone()
    };
    for (rel, tuples) in &base.facts {
        for (i, tu) in tuples.iter().enumerate() {
            let key = tu[1].as_time().unwrap();
            if rel == "close" && i % 10 == 3 {
                let k = bars.iter().position(|&b| b == key).unwrap();
                let avail = bars.get(k + 1).copied().unwrap_or(key);
                ds.add_available(rel, tu.clone(), avail);
            } else {
                ds.add(rel, tu.clone());
            }
        }
    }
    ds
}

#[test]
fn the_fold_is_causal_on_receipt() {
    let p = common::corpus_program("sma_crossover");
    let ds = delayed();
    assert!(ds.has_availability());
    let client = common::client(&p, &ds);
    let full = run_on(&p, None, &client, ExecConfig::frictionless(), &Replay::default()).unwrap();
    let prompt = run(&p, &on_time(), ExecConfig::frictionless()).unwrap();
    let describe = |r: &abt_kernel::kernel::RunResult, t: i64| -> Vec<String> {
        let mut v: Vec<String> = r.decisions_at(t).into_iter().map(|d| r.describe_decision(d)).collect();
        v.sort();
        v
    };
    let mut differs = 0;
    for &t in &full.bars {
        // What was received by t's barrier: the stream truncated there
        // decides at t as the whole stream did (the executor's own fills
        // honour receipt too).
        let partial = run_on(
            &p,
            None,
            &client,
            ExecConfig::frictionless(),
            &Replay {
                horizon: Some(t),
                ..Replay::default()
            },
        )
        .unwrap();
        assert_eq!(describe(&full, t), describe(&partial, t), "at {}", format_timestamp(t));
        if describe(&full, t) != describe(&prompt, t) {
            differs += 1;
        }
    }
    assert!(differs > 0, "the late closes, received on time, would have changed decisions");
    // The empirical check of the theorem.
    let samples: Vec<i64> = [50usize, 100, 150, 199].iter().map(|&i| full.bars[i]).collect();
    assert!(verify_causality(&p, &ds, ExecConfig::frictionless(), &samples).unwrap().is_empty());
}

#[test]
fn a_late_tuple_is_read_from_its_availability_on() {
    // One name, prices 10, 11, 12, 13, 14; the close of day 2 arrives at the
    // close of day 3. A rule reading today's close sees nothing on day 2; a
    // rule reading yesterday's close sees it on day 3. The name is in the
    // universe while listed, so day 2 is a bar whatever its close.
    let days: Vec<i64> = ["2024-01-08", "2024-01-09", "2024-01-10", "2024-01-11", "2024-01-12"].iter().map(|d| common::day(d)).collect();
    let mut ds = Dataset::new();
    let x = ds.add_security("X", "X", days[0], None);
    for (i, &t) in days.iter().enumerate() {
        let p = 10.0 + i as f64;
        if i == 1 {
            for rel in ["open", "high", "low"] {
                ds.add(rel, vec![Value::Equity(x), Value::Time(t), Value::Num(p)]);
            }
            ds.add_available("close", vec![Value::Equity(x), Value::Time(t), Value::Num(p)], days[2]);
            ds.add("volume", vec![Value::Equity(x), Value::Time(t), Value::Num(1000.0)]);
            ds.add("listed", vec![Value::Equity(x), Value::Time(t)]);
        } else {
            common::add_bar(&mut ds, x, t, p, 1000.0);
        }
    }
    ds.derive_tickers();
    let src = common::StrategyParts {
        name: "reads".into(),
        universe: "universe(A, T) :- listed(A, T).".into(),
        params: "    w : Scalar = 0.1".into(),
        rules: r#"
    rel today(@T)
    today(T) :- universe(A, T), close(A, T, _).
    rel yesterday(@T)
    yesterday(T) :- universe(A, T), prev(T, T0), close(A, T0, _).
"#
        .into(),
        decides: "    decide(T, hold(A, w)) :- universe(A, T), today(T), yesterday(T).".into(),
        ..Default::default()
    }
    .render();
    let p = common::program(&src, "reads");
    let r = run_socket(
        &p,
        &ds,
        ExecConfig {
            initial_cash: 1000.0,
            ..ExecConfig::frictionless()
        },
    )
    .unwrap();
    let decided: Vec<String> = r.decisions.iter().map(|d| format_timestamp(d.t)).collect();
    // Day 2: its close is not available, so `today` fails. Day 3: today's
    // close and, by then, yesterday's late one. Days 4 and 5 as usual.
    assert_eq!(decided, vec!["2024-01-10", "2024-01-11", "2024-01-12"], "{:?}", decided);
    // Received at its bar, the close of day 2 is read on day 2.
    let mut prompt = ds.clone();
    prompt.available_at.clear();
    let b = run(
        &p,
        &prompt,
        ExecConfig {
            initial_cash: 1000.0,
            ..ExecConfig::frictionless()
        },
    )
    .unwrap();
    assert_eq!(b.decisions.len(), 4);
    // The first decision, day 3's, fills at day 4's close.
    assert!((r.fills[0].price - 13.0).abs() < 1e-9 && format_timestamp(r.fills[0].t) == "2024-01-11", "{:?}", r.fills);
}

#[test]
fn availability_loads_from_parquet_and_truncation_follows_it() {
    let p = common::corpus_program("sma_crossover");
    let ds = delayed();
    let dir = std::env::temp_dir().join(format!("abt-avail-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    write_parquet_dir(&p, &ds, &dir).unwrap();
    let close = abt_kernel::table::to_text(&dir.join("close.parquet")).unwrap();
    assert!(close.lines().next().unwrap().ends_with(",available_at"), "{}", close.lines().next().unwrap());
    let (loaded, _) = abt_tests::load_fixture(&p, &dir).unwrap();
    assert!(loaded.has_availability());
    assert_eq!(loaded.availability_of("close").unwrap().len(), ds.facts["close"].len());
    let a = run_socket(&p, &ds, ExecConfig::frictionless()).unwrap();
    let b = run_socket(&p, &loaded, ExecConfig::frictionless()).unwrap();
    assert_eq!(a.decisions.len(), b.decisions.len());
    assert_eq!(a.final_cash.to_bits(), b.final_cash.to_bits());
    // The receipt filter at t's barrier keeps what was received by then, not
    // what was keyed at t (D-4.2).
    let t = a.bars[33];
    let keyed: usize = ds.facts["close"].iter().filter(|tu| tu[1].as_time().unwrap() <= t).count();
    let client = common::client(&p, &ds);
    let count = |r_max: Option<i64>| -> i64 {
        use abt_wire::generated::abt::wire::{CountRows, Message, QueryForm, QueryRequest, ReceiptFilter};
        let form = QueryForm::CountRows(Box::new(CountRows {
            relation: "equity.close".into(),
            t0: i64::MIN,
            t1: t,
            predicate: None,
        }));
        let b = client
            .table(Message::QueryRequest(Box::new(QueryRequest {
                view: None,
                form,
                receipt_filter: Some(Box::new(ReceiptFilter { r_max })),
                with_seq: false,
                explain: false,
                ticks: false,
            })))
            .unwrap();
        use arrow::array::AsArray;
        b[0].column(0).as_primitive::<arrow::datatypes::Int64Type>().value(0)
    };
    assert_eq!(count(None) as usize, keyed);
    let received = count(Some(abt_kernel::kernel::bind::barrier_at(&p, t))) as usize;
    assert!(received < keyed, "{} received of {} keyed at {}", received, keyed, format_timestamp(t));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_revision_replaces_the_row_it_revises() {
    // Two names over six days. X's close of the second day, 10, is revised
    // to 30, received on the fourth (K-4.2). A decision before that read 10
    // and one after reads 30, and the bar holds one row of each name.
    let src = common::strategy(
        "revised",
        "    unit : Quantity<Shares> = 1 shares\n    ten : Price<USD> = 10 USD/share",
        "    rel peak(A: Equity, @T, M: Price<USD>)\n    peak(A, T, M) :- universe(A, T), M = max(P) over (T1 in window[10d] min 1, close(A, T1, P)).",
        "    decide(T, trade(A, Q)) :- peak(A, T, M), Q = M / ten * unit.",
    );
    let p = common::program(&src, "revised");
    let mut ds = common::markets((2024, 1, 8), &[("X", &[10.0; 6]), ("Y", &[20.0; 6])]);
    let days = business_days((2024, 1, 8), 6);
    let x = ds.symbols.get("X").unwrap();
    ds.add_available("close", vec![Value::Equity(x), Value::Time(days[1]), Value::Num(30.0)], days[3]);
    let cfg = ExecConfig {
        pushdown: false,
        ..ExecConfig::frictionless()
    };
    let (r, mut k) = common::run_kernel(&p, &ds, cfg).unwrap();
    let amount = |t: i64, name: &str| r.decisions.iter().find(|d| d.t == t && r.describe_decision(&d.decision).contains(name)).map(|d| d.decision.amount);
    assert_eq!(amount(days[1], "X"), Some(1.0));
    assert_eq!(amount(days[4], "X"), Some(3.0));
    assert_eq!(amount(days[4], "Y"), Some(2.0));
    let names = k.symbol_names().to_vec();
    let mut held: Vec<(String, f64)> = k
        .query("close", days[1], &[])
        .unwrap()
        .iter()
        .map(|tu| (names[tu[0].as_equity().unwrap() as usize].clone(), tu[2].as_f64().unwrap()))
        .collect();
    held.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(held, vec![("X".to_string(), 30.0), ("Y".to_string(), 20.0)]);
}

/// A kernel bound over `ds` and the events of its stream.
fn kernel_and_events<'p>(p: &'p Program, ds: &Dataset, cfg: ExecConfig) -> (Kernel<'p>, Vec<StreamEvent>) {
    let client = common::client(p, ds);
    let bound = bind(p, &client).unwrap();
    let mut k = Kernel::bound(p, &bound, cfg).unwrap();
    let (spec, cal_of) = stream_spec(&k, &bound, &client, "test").unwrap();
    let mut decoder = Decoder::new(&k, &bound, cal_of);
    let mut sub = client.subscribe(spec, 1, None).unwrap();
    let mut events = Vec::new();
    while let Some(b) = sub.next_batch().unwrap() {
        events.extend(decoder.events(&mut k, &bound, &b).unwrap());
    }
    (k, events)
}

#[test]
fn a_tuple_is_late_exactly_when_its_key_is_at_or_before_the_last_decided_label() {
    // A weekly book over four weeks of daily bars: the decision calendar's
    // labels are the weeks', and a daily key falls before, on or between
    // them. K-4.4 calls a tuple late when its decision bucket, the first
    // label at or after its key, was decided.
    let src = common::StrategyParts {
        name: "weekly".into(),
        calendars: Some("weekly = weeks(1)".into()),
        clock: "decide on weekly".into(),
        universe: "universe(A, T) :- traded@weekly(A, T).".into(),
        params: "    qty : Quantity<Shares> = 1 shares".into(),
        decides: "    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T).".into(),
        report: "calendar weekly   periods 52 per year".into(),
        ..Default::default()
    }
    .render();
    let p = common::program(&src, "weekly");
    let ds = common::market_from("X", (2024, 1, 8), &[10.0; 20]);
    let cfg = ExecConfig::frictionless();
    let (k, events) = kernel_and_events(&p, &ds, cfg.clone());
    let decided = |e: &StreamEvent| match e {
        StreamEvent::Barrier { cal, label, at, .. } if *cal == p.decision_cal => Some((*label, *at)),
        _ => None,
    };
    let labels: Vec<i64> = events.iter().filter_map(decided).map(|(label, _)| label).collect();
    assert_eq!(labels.len(), 4, "four weeks: {:?}", labels);
    let close = k.relation_id("close").unwrap();
    let a_close = events
        .iter()
        .find_map(|e| match e {
            StreamEvent::Tuple { rel, tuple, .. } if *rel == close => Some(tuple.clone()),
            _ => None,
        })
        .unwrap();
    let bucket_decided = |key: i64, last: i64| labels.iter().find(|l| **l >= key).is_some_and(|l| *l <= last);
    // With a label of the schedule decided last, the bucket was decided
    // exactly when the key is at or before that label, whichever label it
    // is and wherever the key lies: before the first label, on one, between
    // two, beyond the last.
    for &last in &labels {
        for &l in &labels {
            for key in [l - 30 * DAY, l - DAY, l - 1, l, l + 1, l + DAY, l + 30 * DAY] {
                assert_eq!(bucket_decided(key, last), key <= last, "key {} with {} decided last", format_timestamp(key), format_timestamp(last));
            }
        }
    }
    // The fold up to the barrier of a label, then closes received after it.
    let fed_to = |k: Kernel<'_>, exec: &mut SimExecutor, last: i64, barrier: Option<i64>, keys: &[i64]| -> Vec<i64> {
        let mut fold = Fold::new(k, exec);
        let mut at = 0;
        for e in &events {
            fold.step(e.clone().into()).unwrap();
            if let Some((label, instant)) = decided(e) {
                at = instant;
                if label == last {
                    break;
                }
            }
        }
        assert_eq!(fold.last_bar(), Some(last));
        assert!(fold.result().late.is_empty(), "every tuple of the market is received at its bar");
        if let Some(label) = barrier {
            let cal = p.decision_cal.clone();
            fold.step(Event::Barrier { seq: 1 << 40, cal, label, at: at + 1 }).unwrap();
            assert_eq!(fold.last_bar(), Some(label));
        }
        for (i, &key) in keys.iter().enumerate() {
            let mut tuple = a_close.clone();
            tuple[1] = Value::Time(key);
            let seq = (1 << 40) + 1 + i as u64;
            fold.step(Event::Tuple {
                seq,
                rel: close,
                key,
                r: at + 2,
                tuple,
            })
            .unwrap();
        }
        assert_eq!(fold.kernel.stats().late_tuples, fold.result().late.len(), "each late tuple is counted and recorded");
        let name = a_close[0].as_equity();
        assert!(name.is_some() && fold.result().late.iter().all(|l| l.relation == "close" && l.entity == name && l.r == at + 2));
        fold.result().late.iter().map(|l| l.t).collect()
    };
    // The second week decided last: keys before it, on it and after it, on
    // a later label, and beyond the schedule's end.
    let last = labels[1];
    let keys = [
        labels[0] - DAY,
        labels[0],
        labels[0] + DAY,
        last - 1,
        last,
        last + 1,
        last + DAY,
        labels[2],
        labels[3],
        labels[3] + 1,
        labels[3] + 30 * DAY,
    ];
    let mut exec = SimExecutor::new(cfg.clone());
    let late = fed_to(k, &mut exec, last, None, &keys);
    assert_eq!(late, keys.iter().copied().filter(|key| *key <= last).collect::<Vec<_>>());
    assert_eq!(late, keys.iter().copied().filter(|key| bucket_decided(*key, last)).collect::<Vec<_>>());
    assert_eq!(late.len(), 5);
    // A barrier whose label the schedule does not hold (a live clock's)
    // decides no bucket of the schedule: a key between the last scheduled
    // label and it is at or before it and still not late, as before.
    let off = labels[2] - 2 * DAY;
    assert!(last < off && !labels.contains(&off));
    let keys = [labels[0], last, last + 1, off - DAY, off, off + 1, labels[2]];
    let (k, _) = kernel_and_events(&p, &ds, cfg.clone());
    let mut exec = SimExecutor::new(cfg);
    let late = fed_to(k, &mut exec, last, Some(off), &keys);
    assert_eq!(late, vec![labels[0], last]);
    assert_eq!(late, keys.iter().copied().filter(|key| bucket_decided(*key, off)).collect::<Vec<_>>());
    assert!(keys.iter().filter(|key| **key <= off).count() > late.len());
}
