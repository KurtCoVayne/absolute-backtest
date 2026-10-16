//! The query language (V-8, K-7.2) over a run: `?` queries checked as rules
//! of a stratum above the program and answered at a bar, over a window or
//! for one instrument; `why`, `why not`, `trace`, `coverage`, the `show`
//! forms and `ledger ... into`; a query changes no decision (Proposition
//! V-8.4).

#![allow(clippy::result_large_err)]

use abt_kernel::check::{check_program, Program, Workspace};
use abt_kernel::kernel::{Dataset, ExecConfig, Replay, RunResult, Value};
use abt_kernel::query::{self, Answer, Query, Show};
use abt_kernel::state::tables::Cell;
use abt_tests as common;

/// A corpus strategy with its workspace and golden market.
fn corpus(name: &str) -> (Workspace, Program, Dataset) {
    let path = common::corpus_dir().join("strategies").join(format!("{}.dsl", name));
    let (_, ds) = common::golden_inputs(&path).unwrap();
    let mut ws = common::base_workspace();
    ws.add_source(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let (prog, diags) = check_program(&ws, name);
    (ws, prog.unwrap_or_else(|| panic!("{}", common::text(&diags))), ds)
}

/// Runs the program prepared for the queries and answers each over the
/// kernel at the end of the run, with what `f` reads besides.
fn ask<T: Send>(
    ws: &Workspace,
    prog: &Program,
    ds: &Dataset,
    texts: &[&str],
    f: impl for<'k> FnOnce(&RunResult, &mut abt_kernel::kernel::Kernel<'k>) -> T + Send,
) -> (RunResult, Vec<Result<Answer, String>>, T) {
    let queries: Vec<Query> = texts.iter().map(|t| query::parse(t).unwrap_or_else(|e| panic!("{}: {}", t, e))).collect();
    let prepared = query::prepare(ws, &prog.strategy, prog, &queries).unwrap_or_else(|e| panic!("{}", e));
    let p = &prepared.program;
    let client = common::client(p, ds);
    let (r, (answers, extra)) = abt_kernel::kernel::run_keeping(p, None, &client, ExecConfig::from_program(p), &Replay::default(), |r, k| {
        let cx = query::Context {
            prog: p,
            result: r,
            prepared: &prepared,
            plan: None,
            out: None,
        };
        let answers = queries.iter().enumerate().map(|(i, q)| query::answer(k, &cx, i, q).map_err(|e| e.to_string())).collect::<Vec<_>>();
        Ok((answers, f(r, k)))
    })
    .unwrap();
    (r, answers, extra)
}

fn rows(a: &Result<Answer, String>) -> Vec<Vec<Cell>> {
    a.as_ref().unwrap_or_else(|e| panic!("{}", e)).table.as_ref().expect("a table").rows.clone()
}

fn cell_text(c: &Cell) -> String {
    c.text()
}

#[test]
fn every_form_parses_and_a_malformed_query_is_refused() {
    let t = |s: &str| abt_kernel::kernel::time::parse_timestamp(s).unwrap();
    match query::parse("? close(A, T, P), P > 1 USD/share at T = 2022-03-01").unwrap() {
        Query::Ask(a) => {
            assert_eq!(a.body, "close(A, T, P), P > 1 USD/share");
            assert_eq!((a.time_var.as_deref(), a.at), (Some("T"), Some(t("2022-03-01"))));
        }
        q => panic!("{:?}", q),
    }
    match query::parse("? close(A, T, P) for A = \"ES\" over 2020-01 .. 2020-03").unwrap() {
        Query::Ask(a) => {
            assert_eq!(a.bind, Some(("A".into(), "ES".into())));
            assert_eq!(a.over, Some((t("2020-01-01"), t("2020-04-01") - 1)));
        }
        q => panic!("{:?}", q),
    }
    assert!(matches!(query::parse("? decide(T, D) at T").unwrap(), Query::Ask(a) if a.every_bar));
    assert_eq!(
        query::parse("why decide(2022-02-16, trade(E2, 100))").unwrap(),
        Query::Why {
            relation: "decide".into(),
            args: vec!["2022-02-16".into(), "trade(E2, 100)".into()]
        }
    );
    assert_eq!(
        query::parse("why not above(\"E2\", 2022-04-07) in written order").unwrap(),
        Query::WhyNot {
            relation: "above".into(),
            args: vec!["E2".into(), "2022-04-07".into()],
            written: true
        }
    );
    assert_eq!(
        query::parse("trace decide at 2022-04-07").unwrap(),
        Query::Trace {
            relation: "decide".into(),
            at: t("2022-04-07")
        }
    );
    assert_eq!(query::parse("coverage").unwrap(), Query::Coverage { over: None });
    assert_eq!(query::parse("show plan above").unwrap(), Query::Show(Show::Plan(Some("above".into()))));
    assert_eq!(query::parse("show calendars").unwrap(), Query::Show(Show::Calendars));
    assert_eq!(
        query::parse("ledger above into above_daily").unwrap(),
        Query::Ledger {
            relation: "above".into(),
            into: "above_daily".into()
        }
    );
    for bad in [
        "",
        "?",
        "why",
        "show nothing",
        "ledger above",
        "trace decide",
        "coverage over 2022-05 .. 2022-01",
        "? close(A, T, P) at 2022-01-03 over 2022-01 .. 2022-02",
        "? close(A, T, P) at someday",
        "select * from close",
    ] {
        assert!(query::parse(bad).is_err(), "`{}` parsed", bad);
    }
}

#[test]
fn an_atom_query_is_the_relations_extension_at_the_bar() {
    let (ws, prog, ds) = corpus("sma_crossover");
    let base = common::run(&prog, &ds, ExecConfig::from_program(&prog)).unwrap();
    let t = base.bars[90];
    let day = abt_kernel::kernel::time::format_timestamp(t);
    let (_, answers, extension) = ask(&ws, &prog, &ds, &[&format!("? above(A, T) at T = {}", day)], |_, k| {
        let mut v: Vec<String> = k.query("above", t, &[]).unwrap().iter().map(|tu| k.show(&tu[0])).collect();
        v.sort();
        v
    });
    let got: Vec<String> = rows(&answers[0]).iter().map(|r| cell_text(&r[0])).collect();
    assert_eq!(got, extension);
    // Each row names the rule that derived it.
    assert!(rows(&answers[0]).iter().all(|r| cell_text(&r[2]) == "sma_crossover::above#1"));
}

#[test]
fn a_conjunctive_query_filters_as_its_body_says() {
    let (ws, prog, ds) = corpus("sma_crossover");
    let base = common::run(&prog, &ds, ExecConfig::from_program(&prog)).unwrap();
    let t = base.bars[120];
    let day = abt_kernel::kernel::time::format_timestamp(t);
    let (_, answers, want) = ask(&ws, &prog, &ds, &[&format!("? above(A, T), close(A, T, P), P > 50 USD/share at T = {}", day)], |_, k| {
        let mut v = Vec::new();
        for tu in k.query("above", t, &[]).unwrap() {
            for c in k.query("close", t, &[]).unwrap() {
                if c[0] == tu[0] && matches!(c[2], Value::Num(p) if p > 50.0) {
                    v.push(k.show(&tu[0]));
                }
            }
        }
        v.sort();
        v
    });
    let got: Vec<String> = rows(&answers[0]).iter().map(|r| cell_text(&r[0])).collect();
    assert_eq!(got, want);
    assert_eq!(
        answers[0].as_ref().unwrap().table.as_ref().unwrap().columns.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(),
        ["A", "T", "P"]
    );
}

#[test]
fn a_query_that_does_not_check_is_refused_with_the_checkers_diagnostic() {
    let (ws, prog, _) = corpus("sma_crossover");
    for (text, code) in [
        ("? close(A, T, P), P > 60 at T = 2022-04-07", "[T]"),
        ("? nowhere(A, T) at T = 2022-04-07", "[U]"),
        ("? close(A, T, P), Q > P at T = 2022-04-07", "Q"),
    ] {
        let q = query::parse(text).unwrap();
        match query::prepare(&ws, "sma_crossover", &prog, &[q]) {
            Err(e) => assert!(e.contains(code), "{}: {}", text, e),
            Ok(_) => panic!("`{}` checked", text),
        }
    }
}

/// Proposition V-8.4: the run holding the queries' rules decides as the
/// program does, to the bit.
#[test]
fn queries_change_no_decision() {
    for name in ["sma_crossover", "momentum_top_n", "pairs_trading"] {
        let (ws, prog, ds) = corpus(name);
        let base = common::run(&prog, &ds, ExecConfig::from_program(&prog)).unwrap();
        let day = abt_kernel::kernel::time::format_timestamp(base.bars[base.bars.len() / 2]);
        let (r, answers, _) = ask(&ws, &prog, &ds, &["? decide(T, D) at T", &format!("? close(A, T, P) at T = {}", day)], |_, _| ());
        assert!(answers.iter().all(|a| a.is_ok()), "{}: {:?}", name, answers.iter().filter_map(|a| a.as_ref().err()).collect::<Vec<_>>());
        assert_eq!(r.decisions.len(), base.decisions.len(), "{}", name);
        assert_eq!(r.final_cash.to_bits(), base.final_cash.to_bits(), "{}", name);
        assert_eq!(rows(&answers[0]).len(), base.decisions.len(), "{}: `? decide(T, D) at T` is every decision", name);
    }
}

#[test]
fn why_why_not_and_trace_explain_a_bar() {
    let (ws, prog, ds) = corpus("sma_crossover");
    let base = common::run(&prog, &ds, ExecConfig::from_program(&prog)).unwrap();
    let d = &base.decisions[0];
    let day = abt_kernel::kernel::time::format_timestamp(d.t);
    let decision = base.describe_decision(&d.decision);
    let sym = base.symbols[d.decision.equity as usize].clone();
    let other = base.symbols.iter().find(|s| **s != sym).unwrap().clone();
    let (_, answers, _) = ask(
        &ws,
        &prog,
        &ds,
        &[
            &format!("why decide({}, {})", day, decision),
            &format!("why not above({}, {})", other, day),
            &format!("why not above({}, {}) in written order", other, day),
            &format!("trace decide at {}", day),
        ],
        |_, _| (),
    );
    let why = &answers[0].as_ref().unwrap().text;
    assert!(why.starts_with(&format!("decide({}, {})", day, decision)), "{}", why);
    assert!(why.contains("supplied by the primitive") && why.contains("sma_crossover::decide#1"), "{}", why);
    for a in &answers[1..3] {
        let text = &a.as_ref().unwrap().text;
        assert!(text.contains("sma_crossover::above#1") && (text.contains("excluded at literal") || text.contains("fired")), "{}", text);
    }
    // One row per rule and instrument; the fired rows are the bar's decisions.
    let trace = rows(&answers[3]);
    assert_eq!(trace.len(), 2 * base.symbols.len());
    let fired: Vec<String> = trace.iter().filter(|r| r[3] == Cell::Bool(true)).map(|r| cell_text(&r[1])).collect();
    let decided: Vec<String> = base.decisions_at(d.t).iter().map(|x| base.symbols[x.equity as usize].clone()).collect();
    assert_eq!(fired, decided);
}

#[test]
fn coverage_names_the_first_empty_relation_with_its_why_not() {
    let src = common::strategy(
        "never",
        "    qty : Quantity<Shares> = 10 shares\n    floor : Price<USD> = 100000 USD/share",
        "    rel rich(A: Equity, @T)\n    rich(A, T) :- universe(A, T), close(A, T, P), P > floor.\n    rel go(A: Equity, @T)\n    go(A, T) :- rich(A, T), flat(A, T).",
        "    decide(T, trade(A, qty)) :- go(A, T).",
    );
    let mut ws = common::base_workspace();
    ws.add_source(&src).unwrap();
    let (prog, diags) = check_program(&ws, "never");
    let prog = prog.unwrap_or_else(|| panic!("{}", common::text(&diags)));
    let ds = abt_kernel::data::synthetic_daily(&["AAA", "BBB"], (2022, 1, 3), 40, 3);
    let (_, answers, _) = ask(&ws, &prog, &ds, &["coverage", "coverage over 2022-02-01 .. 2022-02-10"], |_, _| ());
    for a in &answers {
        let a = a.as_ref().unwrap();
        assert!(a.text.contains("rich"), "{}", a.text);
        assert!(a.text.contains("why not rich"), "{}", a.text);
        let table = a.table.as_ref().unwrap();
        let rich = table.rows.iter().find(|r| cell_text(&r[0]) == "rich").unwrap();
        assert_eq!(rich[5], Cell::Bool(true), "rich is empty");
    }
    // The window holds its own bars only.
    let all = rows(&answers[0]);
    let window = rows(&answers[1]);
    let bars = |t: &Vec<Vec<Cell>>| t.iter().find(|r| cell_text(&r[0]) == "universe").map(|r| r[2].clone()).unwrap();
    assert_eq!(bars(&window), Cell::Int(8));
    assert_ne!(bars(&all), bars(&window));
}

#[test]
fn the_show_forms() {
    let (ws, prog, ds) = corpus("sma_crossover");
    let (r, answers, _) = ask(&ws, &prog, &ds, &["show program", "show plan above", "show schedule", "show calendars", "show contract"], |_, _| ());
    let text = |i: usize| answers[i].as_ref().unwrap().text.clone();
    assert!(text(0).contains("decide"), "{}", text(0));
    assert!(text(1).contains("sma_crossover::above#1") && !text(1).contains("sma_crossover::below#1"), "{}", text(1));
    assert!(text(2).contains("closed loop") && text(2).contains("flat"), "{}", text(2));
    let cals = rows(&answers[3]);
    assert_eq!(cals.len(), 1);
    assert_eq!(cals[0][1], Cell::Int(r.bars.len() as i64));
    assert!(text(4).contains("inv.nav") && text(4).contains("checked"), "{}", text(4));
    // `show plan R` of a relation the program does not have is refused.
    let (_, answers, _) = ask(&ws, &prog, &ds, &["show plan nowhere"], |_, _| ());
    assert!(answers[0].as_ref().is_err_and(|e| e.contains("nowhere")));
}

/// `ledger R into NAME` writes R's extension over the run through the data
/// layer, as `kernel.<run>.NAME`.
#[test]
fn a_ledger_is_written_into_the_data_layer() {
    let (ws, prog, ds) = corpus("sma_crossover");
    let q = query::parse("ledger above into above_daily").unwrap();
    let prepared = query::prepare(&ws, "sma_crossover", &prog, std::slice::from_ref(&q)).unwrap();
    let client = common::client(&prog, &ds);
    let (r, (answer, want)) = abt_kernel::kernel::run_keeping(&prog, None, &client, ExecConfig::from_program(&prog), &Replay::default(), |r, k| {
        let mut want = 0;
        for &t in &r.bars {
            want += k.query("above", t, &[])?.len();
        }
        let cx = query::Context {
            prog: &prog,
            result: r,
            prepared: &prepared,
            plan: None,
            out: Some((&client, &r.run_id)),
        };
        Ok((query::answer(k, &cx, 0, &q), want))
    })
    .unwrap();
    let answer = answer.unwrap();
    assert!(answer.text.starts_with(&format!("{} rows of above to kernel.{}.above_daily", want, r.run_id)), "{}", answer.text);
    let view = abt_wire::generated::abt::wire::View {
        versions: None,
        binding: None,
        as_of_seq: None,
    };
    let form = abt_wire::generated::abt::wire::QueryForm::Scan(Box::new(abt_wire::generated::abt::wire::Scan {
        relation: format!("kernel.{}.above_daily", r.run_id),
        t0: i64::MIN,
        t1: i64::MAX,
        predicate: None,
        columns: None,
    }));
    let n: usize = client.query(&view, form).unwrap().iter().map(|b| b.num_rows()).sum();
    assert_eq!(n, want);
}
