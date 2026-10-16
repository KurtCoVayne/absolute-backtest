//! The run's outputs (K-7.1, P-4): every relation a run writes under
//! `kernel.<run>.*` reads back through the data layer as the run holds it;
//! a halted run writes what it has, with `status = halted(reason)`; an
//! observed relation's ledger is its extension at every bar of the run, its
//! stuck instances flagged rather than halting the run.

#![allow(clippy::result_large_err)]

use abt_kernel::kernel::{outputs, DataClient, EventKind, ExecConfig, Replay, RunError, RunResult};
use abt_tests as common;
use abt_tests::{golden_inputs, StrategyParts};
use abt_wire::generated::abt::wire::{QueryForm, Scan, View};
use arrow::array::{Array, AsArray, RecordBatch};
use arrow::datatypes::{Float64Type, TimestampNanosecondType, UInt64Type};

/// A relation of the store, every row, in the store's order (t, identity).
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
    // A relation without rows answers no batch.
    let Some(schema) = batches.first().map(|b| b.schema()) else {
        return RecordBatch::new_empty(std::sync::Arc::new(arrow::datatypes::Schema::empty()));
    };
    arrow::compute::concat_batches(&schema, &batches).unwrap()
}

fn nums(b: &RecordBatch, col: &str) -> Vec<Option<f64>> {
    let a = b.column_by_name(col).unwrap_or_else(|| panic!("no column {}", col)).as_primitive::<Float64Type>();
    (0..a.len()).map(|i| (!a.is_null(i)).then(|| a.value(i))).collect()
}

fn ids(b: &RecordBatch, col: &str) -> Vec<Option<u64>> {
    let a = b.column_by_name(col).unwrap_or_else(|| panic!("no column {}", col)).as_primitive::<UInt64Type>();
    (0..a.len()).map(|i| (!a.is_null(i)).then(|| a.value(i))).collect()
}

fn times(b: &RecordBatch, col: &str) -> Vec<i64> {
    let a = b.column_by_name(col).unwrap_or_else(|| panic!("no column {}", col)).as_primitive::<TimestampNanosecondType>();
    (0..a.len()).map(|i| a.value(i)).collect()
}

/// A label or a text column's values (a label answers as a dictionary).
fn texts(b: &RecordBatch, col: &str) -> Vec<Option<String>> {
    let c = b.column_by_name(col).unwrap_or_else(|| panic!("no column {}", col));
    let c = arrow::compute::cast(c, &arrow::datatypes::DataType::Utf8).unwrap();
    let s = c.as_string::<i32>();
    (0..s.len()).map(|i| (!s.is_null(i)).then(|| s.value(i).to_string())).collect()
}

fn bytes(b: &RecordBatch, col: &str) -> Vec<Option<String>> {
    let a = b.column_by_name(col).unwrap_or_else(|| panic!("no column {}", col)).as_binary::<i32>();
    (0..a.len()).map(|i| (!a.is_null(i)).then(|| String::from_utf8(a.value(i).to_vec()).unwrap())).collect()
}

fn bools(b: &RecordBatch, col: &str) -> Vec<Option<bool>> {
    let a = b.column_by_name(col).unwrap_or_else(|| panic!("no column {}", col)).as_boolean();
    (0..a.len()).map(|i| (!a.is_null(i)).then(|| a.value(i))).collect()
}

/// A run over a data service that writes its outputs there.
fn run_writing(prog: &abt_kernel::check::Program, client: &DataClient, cfg: ExecConfig) -> Result<RunResult, RunError> {
    let opts = Replay {
        write_outputs: true,
        ..Default::default()
    };
    abt_kernel::kernel::run_on(prog, None, client, cfg, &opts)
}

/// The rows of a relation in the order of a column of sequence numbers.
fn by_seq(b: &RecordBatch) -> RecordBatch {
    let seq = b.column_by_name("seq").unwrap();
    let order = arrow::compute::sort_to_indices(seq, None, None).unwrap();
    arrow::compute::take_record_batch(b, &order).unwrap()
}

#[test]
fn every_relation_reads_back_as_the_run_holds_it() {
    for name in ["sma_crossover", "dividend_capture", "pairs_trading"] {
        let (prog, ds) = golden_inputs(&common::corpus_dir().join("strategies").join(format!("{}.dsl", name))).unwrap();
        let client = common::client(&prog, &ds);
        let r = run_writing(&prog, &client, ExecConfig::from_program(&prog)).unwrap();
        let run = r.run_id.clone();
        assert_eq!(run.len(), 16, "{}: the run id", name);
        let rel = |n: &str| scan(&client, &format!("kernel.{}.{}", run, n));
        // The event log, every event in the order of the log.
        let ev = by_seq(&rel("events"));
        assert_eq!(ev.num_rows(), r.events.len(), "{}", name);
        assert_eq!(ids(&ev, "seq"), r.events.iter().map(|e| Some(e.seq)).collect::<Vec<_>>());
        assert_eq!(texts(&ev, "kind"), r.events.iter().map(|e| Some(e.kind.name().to_string())).collect::<Vec<_>>());
        assert_eq!(nums(&ev, "cash_flow"), r.events.iter().map(|e| e.cash_flow).collect::<Vec<_>>());
        // The log's cash flows fold from the initial capital to the final cash.
        let cash = nums(&ev, "cash_flow").iter().flatten().fold(ExecConfig::from_program(&prog).initial_cash, |c, f| c + f);
        assert!((cash - r.final_cash).abs() <= 1e-9 * r.final_cash.abs().max(1.0), "{}: {} vs {}", name, cash, r.final_cash);
        // The book at every decision barrier.
        let book = rel("book");
        assert_eq!(times(&book, "t"), r.exposure.iter().map(|x| x.t).collect::<Vec<_>>(), "{}", name);
        assert_eq!(nums(&book, "nav"), r.exposure.iter().map(|x| Some(x.equity)).collect::<Vec<_>>());
        assert_eq!(nums(&book, "receivables"), r.exposure.iter().map(|x| Some(x.receivables)).collect::<Vec<_>>());
        assert_eq!(rel("holdings").num_rows(), r.holdings.len(), "{}", name);
        // The projections.
        let dec = by_seq(&rel("decisions"));
        assert_eq!(dec.num_rows(), r.decisions.len(), "{}", name);
        assert_eq!(nums(&dec, "amount"), r.decisions.iter().map(|d| Some(d.decision.amount)).collect::<Vec<_>>());
        assert_eq!(texts(&dec, "ctor"), r.decisions.iter().map(|d| Some(d.decision.ctor.resolved_name().to_string())).collect::<Vec<_>>());
        let fills = by_seq(&rel("fills"));
        assert_eq!(fills.num_rows(), r.fills.len(), "{}", name);
        assert_eq!(nums(&fills, "price"), r.fills.iter().map(|f| Some(f.price)).collect::<Vec<_>>());
        assert_eq!(nums(&fills, "quantity"), r.fills.iter().map(|f| Some(f.quantity)).collect::<Vec<_>>());
        // A fill's instrument is the event's.
        let fill_events: Vec<Option<u64>> = ev
            .column_by_name("kind")
            .map(|_| {
                texts(&ev, "kind")
                    .iter()
                    .zip(ids(&ev, "instrument"))
                    .filter(|(k, _)| matches!(k.as_deref(), Some("fill" | "partial_fill")))
                    .map(|(_, i)| i)
                    .collect()
            })
            .unwrap();
        assert_eq!(ids(&fills, "instrument"), fill_events);
        assert_eq!(rel("coverage").num_rows(), r.coverage.len(), "{}", name);
        // The contract with the run's statuses.
        let k = rel("contract");
        assert_eq!(k.num_rows(), r.contract.clauses.len());
        for (clause, status) in texts(&k, "clause").iter().zip(texts(&k, "status")) {
            let c = r.contract.get(clause.as_deref().unwrap()).unwrap();
            let word = c.status.to_string().split(|ch: char| !(ch.is_alphanumeric() || ch == '_')).next().unwrap().to_string();
            assert_eq!(status.unwrap(), word, "{}: {}", name, c.id);
        }
        // β with the program's hash and normal form.
        let b = rel("binding");
        assert!(b.num_rows() >= 2, "{}", name);
        let nir = abt_kernel::compiler::normalise(&prog);
        assert!(texts(&b, "program_hash").iter().all(|h| h.as_deref() == Some(nir.hash.as_str())));
        assert!(texts(&b, "relation").iter().any(|r| r.as_deref().is_some_and(|r| r.ends_with(".close"))), "{}", name);
        let plans = rel("plans");
        assert_eq!(plans.num_rows(), prog.rules.iter().map(|r| r.body.len()).sum::<usize>(), "{}", name);
        assert_eq!(rel("late").num_rows(), r.late.len());
        let report = rel("report");
        let keys = texts(&report, "key");
        let status = keys.iter().position(|k| k.as_deref() == Some("status")).unwrap();
        assert_eq!(bytes(&report, "value")[status].as_deref(), Some("\"completed\""));
        assert!(keys.iter().any(|k| k.as_deref() == Some("final_cash")));
        // The relations named are the relations written, and the calendars.
        let written: Vec<String> = client
            .schemas(Some("kernel"))
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .filter(|n| n.starts_with(&format!("kernel.{}.", run)))
            .collect();
        for n in outputs::RELATIONS {
            assert!(written.contains(&format!("kernel.{}.{}", run, n)), "{}: {} not written", name, n);
        }
        assert!(written.contains(&format!("kernel.{}.calendar_label", run)));
        // A dividend's receivable is in the book between its ex-date and
        // its payment, and its events are in the log.
        if name == "dividend_capture" {
            assert!(nums(&book, "receivables").iter().any(|x| x.unwrap_or(0.0) != 0.0));
            assert!(r.events.iter().any(|e| matches!(e.kind, EventKind::DividendDeclared { .. })));
        }
    }
}

/// The run's identity is a function of the program, its configuration and
/// its binding: the same run twice writes the same rows under the same name.
#[test]
fn the_same_run_writes_the_same_outputs() {
    let (prog, ds) = golden_inputs(&common::corpus_dir().join("strategies").join("sma_crossover.dsl")).unwrap();
    let client = common::client(&prog, &ds);
    let a = run_writing(&prog, &client, ExecConfig::from_program(&prog)).unwrap();
    let first = scan(&client, &format!("kernel.{}.events", a.run_id));
    let b = run_writing(&prog, &client, ExecConfig::from_program(&prog)).unwrap();
    assert_eq!(a.run_id, b.run_id);
    assert_eq!(scan(&client, &format!("kernel.{}.events", b.run_id)), first);
    // Another parameter point is another run.
    let mut cfg = ExecConfig::from_program(&prog);
    cfg.param_overrides = vec![("fast".into(), abt_kernel::syntax::parse_lit("15d").unwrap())];
    let c = run_writing(&prog, &client, cfg).unwrap();
    assert_ne!(c.run_id, a.run_id);
}

#[test]
fn a_halted_run_writes_what_it_has() {
    let (prog, ds) = golden_inputs(&common::corpus_dir().join("strategies").join("sma_crossover.dsl")).unwrap();
    let base = common::run(&prog, &ds, ExecConfig::from_program(&prog)).unwrap();
    let at = base.bars[30];
    let cfg = ExecConfig {
        inject: Some(("inv.nav".to_string(), at)),
        ..ExecConfig::from_program(&prog)
    };
    let client = common::client(&prog, &ds);
    match run_writing(&prog, &client, cfg) {
        Err(RunError::Invariant { clause, .. }) => assert_eq!(clause, "inv.nav"),
        Err(e) => panic!("halted with another error: {}", e),
        Ok(_) => panic!("the fault did not halt the run"),
    }
    let names: Vec<String> = client.schemas(Some("kernel")).unwrap().into_iter().map(|s| s.name).collect();
    let report = names.iter().find(|n| n.ends_with(".report")).expect("the halted run's report");
    let run = report.trim_start_matches("kernel.").trim_end_matches(".report").to_string();
    let r = scan(&client, report);
    let keys = texts(&r, "key");
    let status = keys.iter().position(|k| k.as_deref() == Some("status")).unwrap();
    let value = bytes(&r, "value")[status].clone().unwrap();
    assert!(value.starts_with("\"halted(") && value.contains("inv.nav"), "{}", value);
    // What it has: the book to the bar before the fault, the log to it.
    let book = scan(&client, &format!("kernel.{}.book", run));
    assert!(book.num_rows() >= 30 && book.num_rows() <= 31, "{} book rows", book.num_rows());
    assert!(times(&book, "t").iter().all(|&t| t <= at));
    let contract = scan(&client, &format!("kernel.{}.contract", run));
    let clauses = texts(&contract, "clause");
    let nav = clauses.iter().position(|c| c.as_deref() == Some("inv.nav")).unwrap();
    assert_eq!(texts(&contract, "status")[nav].as_deref(), Some("violated"));
}

/// A program observing a relation its decisions read and one whose every
/// instance is stuck on a division by zero, which no decision demands.
fn observed() -> String {
    StrategyParts {
        name: "observed".into(),
        params: "    qty : Quantity<Shares> = 10 shares".into(),
        rules: [
            "    rel up(A: Equity, @T)",
            "    up(A, T) :- universe(A, T), log_price_return(A, T, R), R > 0.",
            "    rel broken(A: Equity, @T, X: Scalar)",
            "    broken(A, T, X) :- universe(A, T), close(A, T, P), X = P / (P - P).",
            "    observe up",
            "    observe broken",
        ]
        .join("\n"),
        decides: "    decide(T, trade(A, qty)) :- up(A, T), flat(A, T).".into(),
        ..Default::default()
    }
    .render()
}

#[test]
fn an_observed_relation_has_its_ledger_with_its_stuck_instances() {
    let prog = common::program(&observed(), "observed");
    assert_eq!(prog.observes.len(), 2);
    let ds = abt_kernel::data::synthetic_daily(&["AAA", "BBB", "CCC"], (2022, 1, 3), 60, 5);
    let client = common::client(&prog, &ds);
    let opts = Replay {
        write_outputs: true,
        ..Default::default()
    };
    // The extension of `up` at every bar, from the kernel at the end of the run.
    let (r, expected) = abt_kernel::kernel::run_keeping(&prog, None, &client, ExecConfig::from_program(&prog), &opts, |r, k| {
        let mut n = Vec::new();
        for &t in &r.bars {
            n.push((t, k.query("up", t, &[])?.len()));
        }
        Ok(n)
    })
    .unwrap();
    assert!(r.contract.get("post.ledgers").is_some_and(|c| matches!(c.status, abt_kernel::contract::Status::Checked { times: 2 })));
    let up = scan(&client, &format!("kernel.{}.ledger.up", r.run_id));
    assert_eq!(up.num_rows(), expected.iter().map(|(_, n)| n).sum::<usize>());
    assert!(up.num_rows() > 0);
    assert!(bools(&up, "stuck").iter().all(|s| *s == Some(false)));
    // Every row of a bar is numbered from 0, and the instruments are ids.
    let bars = times(&up, "bar");
    for (t, n) in &expected {
        let rows: Vec<usize> = (0..bars.len()).filter(|&i| bars[i] == *t).collect();
        assert_eq!(rows.len(), *n, "the ledger at {}", t);
        assert_eq!(rows.iter().map(|&i| ids(&up, "row")[i]).collect::<Vec<_>>(), (0..*n as u64).map(Some).collect::<Vec<_>>());
    }
    assert!(ids(&up, "A").iter().all(|a| a.is_some()));
    // Every instance of `broken` is stuck, flagged with its halt, and the run
    // went on (Definition V-8.3: no decision demanded it).
    let broken = scan(&client, &format!("kernel.{}.ledger.broken", r.run_id));
    assert!(broken.num_rows() > 0);
    assert!(bools(&broken, "stuck").iter().all(|s| *s == Some(true)));
    assert!(
        bytes(&broken, "stuck_literal").iter().all(|s| s.as_deref().is_some_and(|s| s.contains("broken"))),
        "{:?}",
        bytes(&broken, "stuck_literal").first()
    );
    assert!(nums(&broken, "X").iter().all(|x| x.is_none()));
    assert!(!r.decisions.is_empty());
}

/// Both books observe relations (`scale` and `latched`; `signal`,
/// `aligned`, `settled` and one market's `m_median`): each run writes a
/// ledger per observation, the `where` applied.
#[test]
fn the_books_write_their_ledgers() {
    for (book, observed) in [("mw14", vec!["scale", "latched"]), ("r8l", vec!["signal", "aligned", "settled", "m_median"])] {
        let (prog, ds) = golden_inputs(&common::corpus_dir().join("company").join(format!("{}.dsl", book))).unwrap();
        let client = common::client(&prog, &ds);
        let r = run_writing(&prog, &client, ExecConfig::from_program(&prog)).unwrap();
        for rel in &observed {
            let l = scan(&client, &format!("kernel.{}.ledger.{}", r.run_id, rel));
            if l.num_rows() > 0 {
                assert!(bools(&l, "stuck").iter().all(|s| s.is_some()), "{} {}", book, rel);
            }
        }
        if book == "r8l" {
            // `observe m_median where A = "ES"`: one market's rows only.
            let l = scan(&client, &format!("kernel.{}.ledger.m_median", r.run_id));
            let markets: std::collections::BTreeSet<Option<u64>> = ids(&l, "A").into_iter().collect();
            assert!(markets.len() <= 1, "{:?}", markets);
        }
        assert!(
            r.contract
                .get("post.ledgers")
                .is_some_and(|c| c.status == abt_kernel::contract::Status::Checked { times: observed.len() as u64 }),
            "{}",
            book
        );
    }
}
