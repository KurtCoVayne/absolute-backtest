//! The state tables and the provenance of a run (compiler design, sections
//! 7.2 to 7.4): every table's schema, text, JSON and Arrow IPC export; the
//! extension, carried and summary tables of a run; one witness per derived
//! tuple and the derivation tree down to supplied tuples; the failure table
//! and why-not with the absence-driven verdict; materialisation of an
//! elided relation; the graph with the compiler's facts.

#![allow(clippy::result_large_err)]

use abt_tests as common;

use std::fs;

use abt_kernel::check::Program;
use abt_kernel::compiler::{compile, CompileOptions};
use abt_kernel::data::synthetic_daily;
use abt_kernel::kernel::{Dataset, ExecConfig, Kernel, Value};
use abt_kernel::observe::show_graph;
use abt_kernel::state::provenance::{derive, parse_id, tuple_id, why_not, witness};
use abt_kernel::state::tables::{self, sampled_bars, Cell, ColType, Table};
use abt_kernel::table::read_ipc_shape;

/// The tickers of the synthetic market. Its securities are `E1` to `E5`
/// carrying them, and a security's id is how the kernel shows an identity.
const SYMS: [&str; 5] = ["AAA", "BBB", "CCC", "DDD", "SPY"];
const IDS: [&str; 5] = ["E1", "E2", "E3", "E4", "E5"];

fn dataset() -> Dataset {
    synthetic_daily(&SYMS, (2022, 1, 3), 120, 7)
}

fn ts(s: &str) -> i64 {
    common::day(s)
}

/// A run under the compiled plan, the kernel kept at the end of the stream.
fn with_run<T: Send>(p: &Program, ds: &Dataset, summaries: bool, f: impl for<'k> FnOnce(&mut Kernel<'k>, &abt_kernel::kernel::RunResult) -> T + Send) -> T {
    with_run_cfg(p, ds, summaries, ExecConfig::default(), f)
}

fn with_run_cfg<T: Send>(p: &Program, ds: &Dataset, summaries: bool, cfg: ExecConfig, f: impl for<'k> FnOnce(&mut Kernel<'k>, &abt_kernel::kernel::RunResult) -> T + Send) -> T {
    let compiled = compile(p, &CompileOptions::default());
    let opts = abt_kernel::kernel::Replay {
        record_summaries: summaries,
        ..Default::default()
    };
    common::run_keeping(p, Some(&compiled.lowered), ds, cfg, &opts, |r, k| Ok(f(k, r))).unwrap().1
}

/// The corpus's z-score mean reversion with one relation of its own that
/// no decision reads: the count of names in the universe at a bar. Only
/// the library relations a decision reaches are instantiated, so this is
/// what R5 elides in a program.
fn zscore_with_probe() -> String {
    common::StrategyParts {
        name: "zscore_probe".into(),
        uses: "calendar, stat, book".into(),
        params: "    n : Duration = 20d\n    k : Count = 12\n    entry : Scalar = -2.0\n    exit : Scalar = 0.0\n    qty : Quantity<Shares> = 100 shares".into(),
        rules: "    rel oversold(A: Equity, @T)\n    oversold(A, T) :- zscore_window[n, k, close](A, T, Z), Z < entry.\n    rel recovered(A: Equity, @T)\n    recovered(A, T) :- zscore_window[n, k, close](A, T, Z), Z > exit.\n    rel probe(@T, N: Count)\n    probe(T, N) :- bar(T), N = count(A) over (universe(A, T)) within (T).".into(),
        decides: "    decide(T, trade(A, qty)) :- oversold(A, T), flat(A, T).\n    decide(T, trade(A, Q)) :- recovered(A, T), held(A, T, Q0), Q = -Q0.".into(),
        ..Default::default()
    }
    .render()
}

fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("abt-state-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// A table renders as text and JSON with its schema, and round-trips
/// through Arrow IPC and Parquet with its columns and row count.
#[test]
fn tables_render_and_export() {
    let mut t = Table::new(
        "probe",
        &[("name", ColType::Str), ("n", ColType::Int), ("x", ColType::Float), ("ok", ColType::Bool), ("bar", ColType::Time)],
    );
    t.push(vec![Cell::Str("a".into()), Cell::Int(1), Cell::Float(0.5), Cell::Bool(true), Cell::Time(ts("2022-01-03"))]);
    t.push(vec![Cell::Null, Cell::Int(2), Cell::Null, Cell::Bool(false), Cell::Time(ts("2022-01-04"))]);
    t.notes.push("two rows".into());
    let text = t.text();
    assert!(text.starts_with("probe(name: text, n: integer, x: number, ok: boolean, bar: timestamp): 2 row(s)\n"), "{}", text);
    assert!(text.contains("2022-01-03") && text.contains("note: two rows"), "{}", text);
    let j = t.json();
    assert_eq!(j["table"], "probe");
    assert_eq!(j["rows"].as_array().unwrap().len(), 2);
    assert_eq!(j["rows"][1][0], serde_json::Value::Null);
    let dir = scratch("export");
    let ipc = dir.join("probe.arrow");
    t.write(&ipc).unwrap();
    let (names, rows) = read_ipc_shape(&ipc).unwrap();
    assert_eq!(names, vec!["name", "n", "x", "ok", "bar"]);
    assert_eq!(rows, 2);
    let pq = dir.join("probe.parquet");
    t.write(&pq).unwrap();
    let back = abt_kernel::table::Table::read(&pq).unwrap();
    assert_eq!(back.len(), 2);
    assert_eq!(back.string(back.col("name").unwrap(), 0).unwrap(), "a");
    assert_eq!(back.time(back.col("bar").unwrap(), 1).unwrap(), ts("2022-01-04"));
    let _ = fs::remove_dir_all(&dir);
}

/// The extension of `decide` is the run's decisions; the carried table of
/// the high-water mark carries each value from an earlier bar; the summary
/// table records every calendar-window group the run served.
#[test]
fn extension_carried_and_summary_of_a_run() {
    let p = common::corpus_program("trailing_stop");
    let ds = dataset();
    with_run(&p, &ds, true, |k, r| {
        assert!(!r.decisions.is_empty(), "the run decides");
        let ext = tables::extension(k, &p, "decide", &r.bars, None).unwrap();
        assert_eq!(ext.len(), r.decisions.len(), "{}", ext.text());
        assert_eq!(ext.columns.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), vec!["T", "D"]);
        assert!(ext.rows.windows(2).all(|w| matches!((&w[0][0], &w[1][0]), (Cell::Time(a), Cell::Time(b)) if a <= b)), "sorted by bar");
        let car = tables::carried(k, &p, "high_since_entry", &r.bars, None).unwrap();
        assert_eq!(car.columns.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), vec!["A", "bar", "carried_from", "H"]);
        assert!(!car.is_empty(), "{}", car.text());
        for row in &car.rows {
            let (Cell::Time(bar), Cell::Time(from)) = (&row[1], &row[2]) else { panic!("{:?}", row) };
            assert!(from < bar, "carried from an earlier bar: {:?}", row);
            assert!(matches!(row[3], Cell::Float(x) if x > 0.0), "{:?}", row);
        }
        // No windowed aggregation runs in this strategy: the summary is empty
        // and says so.
        let sum = tables::summary(k, &p);
        assert!(sum.is_empty() && sum.notes[0].contains("no summary recorded"), "{}", sum.text());
        // A relation the program does not have is refused by name. (A
        // signature carries no inputs in v2, so no relation needs any.)
        let err = tables::extension(k, &p, "nothing", &r.bars, None).unwrap_err();
        assert!(err.to_string().contains("`nothing` is not a relation"), "{}", err);
        let dir = scratch("tables");
        ext.write(&dir.join("decide.arrow")).unwrap();
        assert_eq!(read_ipc_shape(&dir.join("decide.arrow")).unwrap().1, ext.len());
        let _ = fs::remove_dir_all(&dir);
    });
}

/// A calendar window's groups are recorded with their rows per bar: the
/// z-score's mean and standard deviation are two library instances, each
/// with its own rule. (The kernel's window state: evaluated here, without
/// pushdown, which leaves an eligible window's state to the data layer and
/// says so in the table's notes.)
#[test]
fn the_summary_table_records_calendar_windows() {
    let p = common::corpus_program("mean_reversion_zscore");
    let ds = dataset();
    with_run(&p, &ds, true, |k, _| {
        let sum = tables::summary(k, &p);
        assert!(sum.notes.iter().any(|n| n.contains("answered by the data layer's aggregate pass")), "{:?}", sum.notes);
    });
    let local = ExecConfig {
        pushdown: false,
        ..ExecConfig::default()
    };
    with_run_cfg(&p, &ds, true, local, |k, _| {
        let sum = tables::summary(k, &p);
        assert!(!sum.is_empty(), "{}", sum.text());
        let labels: std::collections::BTreeSet<String> = sum.rows.iter().filter_map(|row| if let Cell::Str(s) = &row[0] { Some(s.clone()) } else { None }).collect();
        assert!(sum.rows.iter().all(|row| matches!(&row[6], Cell::Str(s) if s == "cached rows")), "{}", sum.text());
        assert_eq!(labels.len(), 2, "one rule per instance: {:?}", labels);
        assert!(
            labels.iter().any(|l| l.contains("mean_window[")) && labels.iter().any(|l| l.contains("std_window[")),
            "rule labels: {:?}",
            labels
        );
        assert!(
            sum.rows.iter().any(|row| matches!(&row[2], Cell::Str(s) if s == "std")) && sum.rows.iter().any(|row| matches!(&row[2], Cell::Str(s) if s == "mean")),
            "{}",
            sum.text()
        );
        assert!(sum.rows.iter().any(|row| matches!(row[5], Cell::Int(n) if n >= 12)), "a full window: {}", sum.text());
    });
}

/// A rows window served by running sums records its shifted moment sums,
/// from which the served mean is recoverable: `mean = shift + Σx / n`.
#[test]
fn the_summary_table_holds_the_running_sums() {
    let src = common::strategy(
        "rows_sum",
        "    qty : Quantity<Shares> = 10 shares",
        "    rel m(A: Equity, @T, M: Price<USD>)\n    m(A, T, M) :- universe(A, T), M = mean(P) over (T1 in rows[10] min 5, close(A, T1, P)).",
        "    decide(T, trade(A, qty)) :- m(A, T, M), close(A, T, P), P > M, flat(A, T).",
    );
    let p = common::program(&src, "rows_sum");
    let ds = dataset();
    with_run(&p, &ds, true, |k, _| {
        let sum = tables::summary(k, &p);
        let running: Vec<&Vec<Cell>> = sum.rows.iter().filter(|row| matches!(&row[6], Cell::Str(s) if s == "running")).collect();
        assert!(!running.is_empty(), "{}", sum.text());
        for row in running.iter().take(50) {
            let (Cell::Int(n), Cell::Float(shift), Cell::Float(sx), Cell::Float(v)) = (&row[5], &row[7], &row[9], &row[14]) else {
                panic!("{:?}", row)
            };
            assert!(*n >= 2);
            assert!((shift + sx / *n as f64 - v).abs() <= 1e-9 * v.abs().max(1.0), "{:?}", row);
        }
        assert!(sum.rows.iter().all(|row| matches!(&row[2], Cell::Str(s) if s == "mean")));
        // A query after the run reaches behind the group's buffer: it is
        // served along the same walk, so the bits equal what the run saw.
        for row in running.iter().filter(|row| matches!(row[5], Cell::Int(n) if n >= 5)).step_by(37).take(12) {
            let (Cell::Str(group), Cell::Time(bar), Cell::Float(v)) = (&row[3], &row[4], &row[14]) else {
                panic!("{:?}", row)
            };
            let sym = group.split(',').next().unwrap().trim();
            let a = k.parse_binding(sym).unwrap();
            let tuples = k.query("m", *bar, &[]).unwrap();
            let got = tuples.iter().filter(|tu| tu[0] == a).find_map(|tu| match &tu[2] {
                Value::Num(x) => Some(*x),
                _ => None,
            });
            assert_eq!(got.map(f64::to_bits), Some(v.to_bits()), "{} at {}: run {} query {:?}", sym, bar, v, got);
        }
    });
}

/// A decision's witness names its rule and body tuples; the derivation
/// tree follows them to supplied tuples, with a note at the depth limit;
/// ids parse and round-trip.
#[test]
fn derivation_witnesses_and_trees() {
    let p = common::corpus_program("trailing_stop");
    let ds = dataset();
    with_run(&p, &ds, false, |k, r| {
        // A sale is a negative delta.
        let sell = r.decisions.iter().find(|d| d.decision.amount < 0.0).expect("a sell");
        let tuples = k.query("decide", sell.t, &[]).unwrap();
        let tu = tuples.iter().find(|tu| matches!(&tu[1], Value::Decision(d) if d.equity == sell.decision.equity)).unwrap().clone();
        let w = witness(k, &p, "decide", sell.t, &tu).unwrap().expect("a witness");
        assert_eq!(w.rule, "trailing_stop::decide#2", "{:?}", w);
        assert!(
            w.body.iter().any(|b| b.starts_with("held(")) && w.body.iter().any(|b| b.starts_with("high_since_entry(")) && w.body.iter().any(|b| b.starts_with("close(")),
            "{:?}",
            w.body
        );
        let id = tuple_id(k, "decide", &tu, sell.t);
        let (rel, bar, args) = parse_id(&id).unwrap();
        assert_eq!((rel.as_str(), bar, args.len()), ("decide", sell.t, 2));
        assert!(args[1].starts_with("trade(") && args[1].contains(", -"), "{:?}", args);
        let tree = derive(k, &p, &id, 2).unwrap();
        assert_eq!(tree.rule.as_deref(), Some("trailing_stop::decide#2"));
        assert!(tree.size() > 4, "{}", tree.text());
        let text = tree.text();
        assert!(text.contains("supplied by the executor") || text.contains("supplied by the primitive"), "{}", text);
        assert!(text.contains("depth limit"), "{}", text);
        assert!(text.contains("not fill("), "the negation is a leaf: {}", text);
        let j = tree.json();
        assert_eq!(j["rule"], "trailing_stop::decide#2");
        assert!(j["body"].as_array().unwrap().len() >= 3);
        // The derivation table lists one witness per tuple at the bar.
        let d = tables::derivation(k, &p, "decide", sell.t, None).unwrap();
        assert_eq!(d.len(), tuples.len());
        assert!(d.rows.iter().all(|row| matches!(&row[2], Cell::Str(s) if s.starts_with("trailing_stop::decide#"))), "{}", d.text());
        // A tuple that is not there, and an id that is not one.
        let none = derive(k, &p, &format!("decide(2022-01-03, trade(E1, 1))@{}", "2022-01-03"), 2).unwrap();
        assert!(none.note.as_deref().unwrap_or("").contains("no such tuple"), "{:?}", none.note);
        assert!(parse_id("decide(x)").is_err());
        assert!(derive(k, &p, "nothing(1)@2022-01-03", 1).is_err());
    });
}

/// The failure table names, for every rule and instrument, the first
/// excluding literal and the bindings at it; why-not reports the same for
/// one instrument, with the absence-driven verdict when a supplied
/// relation has no tuple there.
#[test]
fn failures_and_why_not() {
    let p = common::corpus_program("trailing_stop");
    let ds = dataset();
    with_run(&p, &ds, false, |k, r| {
        let t = *r.bars.last().unwrap();
        let f = tables::failure(k, &p, "decide", t, None, None).unwrap();
        assert_eq!(
            f.columns.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            vec!["rule", "identity", "bar", "literal", "literal_text", "bindings", "absence_driven"]
        );
        assert!(f.len() >= IDS.len(), "{}", f.text());
        assert!(f.rows.iter().all(|row| matches!(&row[1], Cell::Str(s) if IDS.contains(&s.as_str()))), "{}", f.text());
        assert!(f.rows.iter().any(|row| matches!(&row[4], Cell::Str(s) if s == "strong(A, T)")), "{}", f.text());
        assert!(f.rows.iter().all(|row| matches!(&row[5], Cell::Str(s) if s.contains("T = "))), "bindings at the literal: {}", f.text());
        let w = why_not(k, &p, "decide", t, None, Some(Value::Equity(0))).unwrap();
        assert_eq!(w.rules.len(), 2);
        assert!(w.rules.iter().all(|f| !f.fired));
        assert!(w.text().starts_with("why not decide(E1) at "), "{}", w.text());
        assert!(!w.absence_driven());
        assert_eq!(w.json()["rules"].as_array().unwrap().len(), 2);
    });
    // Absence-driven: a rule excluded by an executor relation with no tuple.
    let src = common::strategy(
        "absent",
        "    qty : Quantity<Shares> = 10 shares",
        "    rel filled(A: Equity, @T)\n    filled(A, T) :- universe(A, T), fill(A, T, _, _).",
        "    decide(T, trade(A, qty)) :- filled(A, T), flat(A, T).",
    );
    let p = common::program(&src, "absent");
    let ds = dataset();
    with_run(&p, &ds, false, |k, r| {
        let t = r.bars[0];
        let w = why_not(k, &p, "filled", t, None, Some(Value::Equity(0))).unwrap();
        assert!(w.absence_driven(), "{}", w.text());
        assert!(w.text().contains("fill(A, T, _, _)") && w.text().contains("absence-driven"), "{}", w.text());
        let f = tables::failure(k, &p, "filled", t, None, None).unwrap();
        assert!(f.rows.iter().all(|row| row[6] == Cell::Bool(true)), "{}", f.text());
    });
}

/// Why-not follows the excluding literal into the derived relation it
/// reads, down to the root: a list the data never carries (the catalog's
/// `SP400`, which the synthetic market has no members of; a name outside
/// the catalog's vocabulary is V), three relations below `decide`, is the
/// absence-driven verdict of the chain (the Oct 6 study's `"SP500"`
/// program, `docs/llm-usability.md`).
#[test]
fn why_not_follows_the_chain_to_the_root() {
    let src = common::StrategyParts {
        name: "chain".into(),
        needs: "need bars @1d   need listing   need lists \"SP400\"".into(),
        params: "    qty : Quantity<Shares> = 10 shares".into(),
        rules: "    rel cand(A: Equity, @T)\n    cand(A, T) :- universe(A, T), member(A, T, \"SP400\").\n    rel sel(A: Equity, @T)\n    sel(A, T) :- cand(A, T), close(A, T, P), P > 0 USD/share."
            .into(),
        decides: "    decide(T, trade(A, qty)) :- sel(A, T), not position(A, T, _).".into(),
        ..Default::default()
    }
    .render();
    let p = common::program(&src, "chain");
    let ds = abt_kernel::data::synthetic_daily_v2(&SYMS, (2022, 1, 3), 60, 7);
    with_run(&p, &ds, false, |k, r| {
        assert!(r.decisions.is_empty());
        let t = *r.bars.last().unwrap();
        let w = why_not(k, &p, "decide", t, None, Some(Value::Equity(0))).unwrap();
        assert_eq!(w.depth(), 3, "{}", w.text());
        assert!(w.absence_driven(), "{}", w.text());
        let roots = w.roots();
        assert_eq!(roots.len(), 1, "{}", w.text());
        assert_eq!(roots[0].rule, "chain::cand#1");
        assert!(roots[0].literal.as_ref().map(|(_, t)| t.contains("member(A, T, \"SP400\")")).unwrap_or(false), "{:?}", roots[0].literal);
        assert!(roots[0].absence_driven);
        let text = w.text();
        assert!(text.contains("    why not sel(") && text.contains("        why not cand("), "{}", text);
        assert!(w.json()["rules"][0]["deeper"]["rules"][0]["deeper"]["relation"] == "cand", "{}", w.json());
        // A chain that ends at a comparison is not absence-driven.
        let w = why_not(k, &p, "sel", t, None, Some(Value::Equity(0))).unwrap();
        assert_eq!(w.depth(), 2);
        assert!(w.absence_driven(), "cand's member literal is the root: {}", w.text());
    });
}

/// Sampled bars, materialisation of an elided relation, and the coverage,
/// plan, book and binding tables.
#[test]
fn materialise_and_the_run_tables() {
    let p = common::program(&zscore_with_probe(), "zscore_probe");
    let ds = dataset();
    let compiled = compile(&p, &CompileOptions::default());
    assert!(compiled.rewritten.elided.contains("probe"), "{:?}", compiled.rewritten.elided);
    with_run(&p, &ds, false, |k, r| {
        let bars = sampled_bars(&r.bars, 10);
        assert!(bars.len() <= 10 && bars.last() == r.bars.last(), "{:?}", bars);
        assert!(bars.windows(2).all(|w| w[0] < w[1]));
        let t = *r.bars.last().unwrap();
        let m = tables::materialise(k, &p, "probe", t, None, true).unwrap();
        assert_eq!(m.name, "materialise(probe)");
        assert_eq!(m.len(), 1, "{}", m.text());
        assert!(m.notes[0].contains("elided"), "{:?}", m.notes);
        let cov = tables::coverage(r);
        assert!(cov.rows.iter().any(|row| matches!(&row[0], Cell::Str(s) if s == "oversold")));
        let plan = tables::plan(&compiled.plan.clone().with_observed(k.literal_counts()));
        assert!(plan.rows.iter().any(|row| matches!(&row[9], Cell::Int(_))), "observed counts: {}", plan.text());
        let book = tables::book(r);
        assert_eq!(book.len(), r.exposure.len());
        let b = tables::binding(&p, &ds);
        assert!(b.rows.iter().any(|row| matches!(&row[0], Cell::Str(s) if s == "close")), "{}", b.text());
        assert!(b.rows.iter().all(|row| matches!(row[2], Cell::Int(n) if n > 0)));
    });
}

/// The graph carries the compiler's facts: the keyedness label of a
/// recursive relation, demanded or elided, the columns' abstract values,
/// the program hash.
#[test]
fn the_graph_shows_the_compiler_facts() {
    let p = common::corpus_program("trailing_stop");
    let compiled = compile(&p, &CompileOptions::default());
    let g = show_graph(&p, &compiled);
    assert!(g.contains("keyed at runtime"), "{}", g);
    assert!(g.contains(", demanded]"), "{}", g);
    assert!(g.contains("columns: A: Entity; T: Timestamp; H: Price<USD>"), "{}", g);
    assert!(g.contains("widened to ±∞ (A010): H"), "{}", g);
    assert!(g.contains(&format!("hash {}", compiled.nir.hash)), "{}", g);
    // Only the library relations a decision reaches are instantiated: what
    // R5 elides is a strategy's own relation nothing demands, listed with
    // its signature.
    let p = common::program(&zscore_with_probe(), "zscore_probe");
    let compiled = compile(&p, &CompileOptions::default());
    let g = show_graph(&p, &compiled);
    assert!(g.contains("elided (R5): relations no demanded"), "{}", g);
    assert!(g.contains("  probe(@T: base, N: Count)"), "the elided relations are listed with their signatures: {}", g);
}
